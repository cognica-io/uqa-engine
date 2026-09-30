//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View removal: the owner check a `DROP VIEW` binds with, and the removal of one view's state once what depends on it is gone.
pub mod context;
pub mod locking;
mod publication;
use context::ViewRemovalContext;

use uqa_sql::{catalog::stored_view::removal as analysis, SQLError};

pub fn ensure_view_drop_authorities(
    context: &ViewRemovalContext<'_>,
    names: &[String],
) -> Result<(), SQLError> {
    let views = context.registry.views_read();
    analysis::ensure_view_drop_authorities(context.ownership, names, &views)
}

/// Remove a view or materialized view with its triggers and rules; nothing else that depends on it.
pub fn drop_view_state_inner(context: &ViewRemovalContext<'_>, name: &str) -> Result<(), SQLError> {
    publication::drop_view_state_inner(
        context.registry,
        context.publication,
        context.events,
        context.changes,
        name,
    )
}
