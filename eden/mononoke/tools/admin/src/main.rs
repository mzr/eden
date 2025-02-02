/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use clap::Parser;
use cmdlib_scrubbing::ScrubArgExtension;
use fbinit::FacebookInit;
use mononoke_app::{MononokeApp, MononokeAppBuilder};

mod commands;

/// Administrate Mononoke
#[derive(Parser)]
struct AdminArgs {}

#[fbinit::main]
fn main(fb: FacebookInit) -> Result<()> {
    let subcommands = commands::subcommands();
    let app = MononokeAppBuilder::new(fb)
        .with_arg_extension(ScrubArgExtension::new())
        .build_with_subcommands::<AdminArgs>(subcommands)?;
    app.run(async_main)
}

async fn async_main(app: MononokeApp) -> Result<()> {
    commands::dispatch(app).await
}
