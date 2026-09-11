//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined routine API, session controls, and canonical SQL type exports.

pub(crate) use uqa_sql::routines::declaration::resolve_plpgsql_datum_types;

pub(crate) use uqa_sql::routines::{
    resolution::RoutineCallKind, routine_local_name, routine_returns_anonymous_record,
    routine_signature_types,
};
pub(crate) use uqa_sql::type_resolution::canonical_routine_type_name;

pub(crate) use uqa_sql::routines::{CompiledFunctionBody, SQLUserFunction};

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
    pub(crate) fn push_sql_notice(&self, level: &str, message: &str) {
        self.query_runtime_view().push_diagnostic(level, message);
    }

    /// Drain queued notices as `(level, message)` pairs in emission
    /// order.
    pub fn take_sql_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.runtime.notices.lock())
    }
}

#[cfg(test)]
mod tests;
