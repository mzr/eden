/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use anyhow::{format_err, Context, Error, Result};
use futures::future::{FutureExt, TryFutureExt};
use futures::stream::{self, StreamExt, TryStreamExt};
use slog::info;

use bookmarks::{
    BookmarkKind, BookmarkName, BookmarkPagination, BookmarkPrefix, Bookmarks, Freshness,
};
use context::CoreContext;
use metaconfig_types::SegmentedChangelogConfig;
use mononoke_types::ChangesetId;

use crate::dag::{NameDagBuilder, VertexListWithOptions, VertexName, VertexOptions};
use crate::idmap::{vertex_name_from_cs_id, IdMap, IdMapWrapper};
use crate::{Group, InProcessIdDag};

#[derive(Debug, Clone)]
pub enum SeedHead {
    Changeset(ChangesetId),
    Bookmark(BookmarkName),
    AllBookmarks,
}

impl From<Option<BookmarkName>> for SeedHead {
    fn from(f: Option<BookmarkName>) -> Self {
        match f {
            None => Self::AllBookmarks,
            Some(n) => Self::Bookmark(n),
        }
    }
}

impl From<BookmarkName> for SeedHead {
    fn from(n: BookmarkName) -> Self {
        Self::Bookmark(n)
    }
}

impl From<ChangesetId> for SeedHead {
    fn from(c: ChangesetId) -> Self {
        Self::Changeset(c)
    }
}

impl From<&ChangesetId> for SeedHead {
    fn from(c: &ChangesetId) -> Self {
        Self::Changeset(*c)
    }
}

impl SeedHead {
    pub async fn into_vertex_list(
        &self,
        ctx: &CoreContext,
        bookmarks: &dyn Bookmarks,
    ) -> Result<VertexListWithOptions> {
        match self {
            Self::Changeset(id) => Ok(VertexListWithOptions::from(vec![head_with_options(id)])),
            Self::AllBookmarks => bookmark_with_options(ctx, None, bookmarks).await,
            Self::Bookmark(name) => bookmark_with_options(ctx, Some(&name), bookmarks).await,
        }
    }
}

impl std::fmt::Display for SeedHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changeset(id) => write!(f, "Bonsai CS {}", id),
            Self::Bookmark(name) => write!(f, "Bookmark {}", name),
            Self::AllBookmarks => write!(f, "All Bookmarks"),
        }
    }
}

pub fn seedheads_from_config(
    ctx: &CoreContext,
    config: &SegmentedChangelogConfig,
) -> Result<Vec<SeedHead>> {
    let head = config
        .master_bookmark
        .as_ref()
        .map(BookmarkName::new)
        .transpose()?
        .into();
    let bonsai_changesets_to_include = &config.bonsai_changesets_to_include;

    info!(ctx.logger(), "using '{}' for head", head);
    if bonsai_changesets_to_include.len() > 0 {
        info!(
            ctx.logger(),
            "also adding {:?} to segmented changelog", bonsai_changesets_to_include
        );
    }

    let mut heads = vec![head];
    heads.extend(bonsai_changesets_to_include.into_iter().map(SeedHead::from));
    Ok(heads)
}

pub async fn vertexlist_from_seedheads(
    ctx: &CoreContext,
    heads: &[SeedHead],
    bookmarks: &dyn Bookmarks,
) -> Result<VertexListWithOptions> {
    let heads_with_options = stream::iter(heads.into_iter().map(Result::Ok))
        .try_fold(VertexListWithOptions::default(), {
            move |acc, head| async move {
                Ok::<_, Error>(acc.chain(head.into_vertex_list(ctx, bookmarks).await?))
            }
        })
        .await?;

    Ok(heads_with_options)
}

pub type ServerNameDag = crate::dag::namedag::AbstractNameDag<InProcessIdDag, IdMapWrapper, (), ()>;

/// Convert a server IdDag and IdMap to a NameDag
/// Note: you will need to call NameDag::map().flush_writes
/// to write out updates to the IdMap
pub fn server_namedag(
    ctx: CoreContext,
    iddag: InProcessIdDag,
    idmap: Arc<dyn IdMap>,
) -> Result<ServerNameDag> {
    let idmap = IdMapWrapper::new(ctx, idmap);
    NameDagBuilder::new_with_idmap_dag(idmap, iddag)
        .build()
        .map_err(anyhow::Error::from)
}

fn head_with_options(head: &ChangesetId) -> (VertexName, VertexOptions) {
    let mut options = VertexOptions::default();
    options.reserve_size = 1 << 26;
    options.highest_group = Group::MASTER;
    (vertex_name_from_cs_id(head), options)
}

async fn bookmark_with_options(
    ctx: &CoreContext,
    bookmark: Option<&BookmarkName>,
    bookmarks: &dyn Bookmarks,
) -> Result<VertexListWithOptions> {
    let bm_stream = match bookmark {
        None => bookmarks
            .list(
                ctx.clone(),
                Freshness::MaybeStale,
                &BookmarkPrefix::empty(),
                BookmarkKind::ALL_PUBLISHING,
                &BookmarkPagination::FromStart,
                u64::MAX,
            )
            .map_ok(|(_bookmark, cs_id)| cs_id)
            .left_stream(),
        Some(bookmark_name) => stream::once(
            bookmarks
                .get(ctx.clone(), bookmark_name)
                .and_then({
                    let bookmark_name = bookmark_name.clone();
                    move |opt_cs_id| async move {
                        opt_cs_id.ok_or_else({
                            move || format_err!("'{}' bookmark could not be found", bookmark_name)
                        })
                    }
                })
                .map({
                    let bookmark_name = bookmark_name.clone();
                    move |r| {
                        r.with_context(|| {
                            format!(
                                "error while fetching changeset for bookmark {}",
                                bookmark_name
                            )
                        })
                    }
                }),
        )
        .right_stream(),
    };
    Ok(VertexListWithOptions::from(
        bm_stream
            .map_ok(|cs| head_with_options(&cs))
            .try_collect::<Vec<_>>()
            .await?,
    ))
}
