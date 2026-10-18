//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The thread-local inputs and cancellation of one running statement, which the statement enters where it runs and each thread that runs part of it enters before that work.

use super::thread_scope::StatementThreadScope;

/// The temporal input a statement's expressions read and the cancellation work inside one scalar evaluation observes. A statement enters the context it runs under for its analysis, planning and execution; work dispatched to another thread captures the dispatching thread's context and enters it on that thread.
#[derive(Clone)]
pub struct StatementThreadContext {
    pub temporal_input: uqa_sql::expr::TemporalInputContext,
    /// The running statement's cancellation token, or `None` for work dispatched from outside any statement.
    pub cancellation: Option<uqa_core::CancellationToken>,
}

impl StatementThreadContext {
    /// Capture the invoking thread's context for work it dispatches to another thread.
    #[must_use]
    pub fn current() -> Self {
        Self {
            temporal_input: uqa_sql::expr::TemporalInputContext::current(),
            cancellation: uqa_sql::expr::statement_cancellation(),
        }
    }

    /// Run the invoking thread under this context until the returned scope drops, which restores the enclosing context.
    #[must_use]
    pub fn enter(&self) -> StatementThreadScope {
        StatementThreadScope::enter(self)
    }
}
