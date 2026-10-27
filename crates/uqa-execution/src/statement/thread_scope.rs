//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore a thread's statement temporal input, cancellation and retained regex cache when the work that entered them ends.

use super::thread_context::StatementThreadContext;

/// Keeps one statement's temporal input, cancellation and retained regex cache entered on the invoking thread until it drops, on every exit of the work it covers.
pub struct StatementThreadScope {
    _retained_rows: crate::query::sources::retained_source::RetainedRowThreadScope,
    _io: uqa_core::io_usage::IOScope,
    _regex_cache: uqa_sql::expr::RegexCacheScope,
    _temporal_input: uqa_sql::expr::TemporalInputScope,
    _cancellation: uqa_sql::expr::StatementCancellationScope,
}

impl StatementThreadScope {
    pub(super) fn enter(context: &StatementThreadContext) -> Self {
        Self {
            _retained_rows: context.retained_rows.enter(),
            _io: context.io.enter(),
            _regex_cache: uqa_sql::expr::RegexCacheScope::enter(context.regex_cache.clone()),
            _temporal_input: context.temporal_input.enter(),
            _cancellation: uqa_sql::expr::StatementCancellationScope::enter(
                context.cancellation.clone(),
            ),
        }
    }
}
