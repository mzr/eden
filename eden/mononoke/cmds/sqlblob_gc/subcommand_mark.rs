/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{ops::Range, sync::Arc};

use anyhow::{anyhow, Context, Result};
use clap::{App, Arg, ArgMatches, SubCommand};
use fbinit::FacebookInit;
use futures::{
    channel::mpsc,
    sink::SinkExt,
    stream::{self, StreamExt, TryStreamExt},
};
use retry::retry;
use slog::{info, Logger};

use sqlblob::Sqlblob;

pub const MARK_SAFE: &str = "mark";
const ARG_INITIAL_GENERATION_ONLY: &str = "initial-generation-only";
const ARG_SKIP_INITIAL_GENERATION: &str = "skip-initial-generation";
const ARG_SKIP_INLINE_SMALL_VALUES: &str = "skip-inline-small-values";

const BASE_RETRY_DELAY_MS: u64 = 1000;
const RETRIES: usize = 3;

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(MARK_SAFE)
        .about("mark referenced blobs as not safe to delete")
        .arg(
            Arg::with_name(ARG_INITIAL_GENERATION_ONLY)
                .long(ARG_INITIAL_GENERATION_ONLY)
                .takes_value(false)
                .required(false)
                .help("Only set generation on blobs that have no generation set yet. Do not do a full sweep.")
        )
        .arg(
            Arg::with_name(ARG_SKIP_INITIAL_GENERATION)
                .long(ARG_SKIP_INITIAL_GENERATION)
                .takes_value(false)
                .required(false)
                .help("Only do the sweep; do not set generation on blobs with no generation set yet.")
        )
        .arg(
            Arg::with_name(ARG_SKIP_INLINE_SMALL_VALUES)
                .long(ARG_SKIP_INLINE_SMALL_VALUES)
                .takes_value(false)
                .required(false)
                .help("Only set the generation, don't inline small values")
        )
}

async fn handle_one_key(
    key: String,
    store: Arc<Sqlblob>,
    inline_small_values: bool,
    logger: Arc<Logger>,
) -> Result<()> {
    retry(
        &logger,
        |_| store.set_generation(&key, inline_small_values),
        BASE_RETRY_DELAY_MS,
        RETRIES,
    )
    .await
    .with_context(|| anyhow!("Failed to handle {} after {} retries", &key, RETRIES))?;
    Ok(())
}

async fn handle_initial_generation(store: &Sqlblob, shard: usize, logger: &Logger) -> Result<()> {
    retry(
        logger,
        |_| store.set_initial_generation(shard),
        BASE_RETRY_DELAY_MS,
        RETRIES,
    )
    .await
    .with_context(|| {
        anyhow!(
            "Failed to handle initial generation on shard {} after {} retries",
            &shard,
            RETRIES
        )
    })?;
    Ok(())
}

pub async fn subcommand_mark<'a>(
    _fb: FacebookInit,
    logger: Logger,
    sub_matches: &'a ArgMatches<'_>,
    max_parallelism: usize,
    sqlblob: Sqlblob,
    shard_range: Range<usize>,
) -> Result<()> {
    if !sub_matches.is_present(ARG_SKIP_INITIAL_GENERATION) {
        info!(logger, "Starting initial generation set");
        let set_initial_generation_futures: Vec<_> = shard_range
            .clone()
            .map(|shard| Ok(handle_initial_generation(&sqlblob, shard, &logger)))
            .collect();
        stream::iter(set_initial_generation_futures.into_iter())
            .try_for_each_concurrent(max_parallelism, |fut| fut)
            .await?;
        info!(logger, "Completed initial generation set");
    }

    if sub_matches.is_present(ARG_INITIAL_GENERATION_ONLY) {
        return Ok(());
    }

    let sqlblob = Arc::new(sqlblob);
    let logger = Arc::new(logger);

    let inline_small_values = !sub_matches.is_present(ARG_SKIP_INLINE_SMALL_VALUES);

    info!(logger, "Starting sweep");
    // Set up a task to process each key in parallel in its own task.
    let (key_channel, processor) = {
        let sqlblob = Arc::clone(&sqlblob);
        let logger = Arc::clone(&logger);
        let (tx, rx) = mpsc::channel(10);
        let task = tokio::spawn(async move {
            rx.map(Ok)
                .try_for_each_concurrent(max_parallelism, {
                    |key| {
                        let sqlblob = sqlblob.clone();
                        let logger = logger.clone();
                        async move {
                            tokio::spawn(handle_one_key(key, sqlblob, inline_small_values, logger))
                                .await?
                        }
                    }
                })
                .await
        });
        (tx, task)
    };

    // Foreach shard in shard_range
    for shard in shard_range {
        info!(logger, "Starting sweep on data keys from shard {}", shard);
        let res = sqlblob
            .get_keys_from_shard(shard)
            .forward(key_channel.clone().sink_err_into())
            .await;
        // Report processing errors ahead of key errors - that way, we don't lose the error if the channel goes away because of an error
        if res.is_err() {
            std::mem::drop(key_channel);
            processor.await??;
            return res;
        }
    }

    // Drop the spare sender so that the processor task can exit
    std::mem::drop(key_channel);

    processor.await??;
    info!(logger, "Completed all sweeps");
    Ok(())
}
