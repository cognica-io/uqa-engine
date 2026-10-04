//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined routine API, session controls, and canonical SQL type exports.

pub(crate) use uqa_sql::routines::SQLUserFunction;

use crate::Engine;
impl Engine {
    /// Current nesting cap for user-defined routine calls.
    pub fn sql_function_depth_limit(&self) -> usize {
        self.runtime
            .function_depth_limit
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adjust the nesting cap for user-defined routine calls
    /// (minimum 1). Mirrors `PostgreSQL`'s `max_stack_depth` role for
    /// recursive functions.
    pub fn set_sql_function_depth_limit(&self, limit: usize) {
        self.runtime
            .function_depth_limit
            .store(limit.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Queue a notice (`RAISE NOTICE` / `WARNING` / ...).
    pub(crate) fn push_sql_notice(&self, notice: uqa_sql::SQLNotice) {
        self.query_runtime_view().push_notice(notice);
    }

    /// Drain the queued notices in emission order, each with its level, SQLSTATE, message, and the detail and hint `PostgreSQL` reports as fields of their own.
    pub fn take_sql_notices(&self) -> Vec<uqa_sql::SQLNotice> {
        self.runtime.notices.take()
    }
}
