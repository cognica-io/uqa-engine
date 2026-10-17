//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore a thread's statement temporal input and cancellation when the work that entered them ends.

use super::thread_context::StatementThreadContext;

/// Keeps one statement's temporal input and cancellation entered on the invoking thread until it drops, on every exit of the work it covers.
pub struct StatementThreadScope {
    _temporal_input: uqa_sql::expr::TemporalInputScope,
    _cancellation: uqa_sql::expr::StatementCancellationScope,
}

impl StatementThreadScope {
    pub(super) fn enter(context: &StatementThreadContext) -> Self {
        Self {
            _temporal_input: context.temporal_input.enter(),
            _cancellation: uqa_sql::expr::StatementCancellationScope::enter(
                context.cancellation.clone(),
            ),
        }
    }
}
