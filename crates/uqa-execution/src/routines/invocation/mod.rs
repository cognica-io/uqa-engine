//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine invocation, scoped state, interpreter entry, and physical result shaping.
mod anonymous;
mod catalog;
pub use catalog::call_catalog_validator;
mod range_support;
pub use range_support::call_range_canonical;
pub mod bodies;
pub mod context;
mod depth;
mod execution;
mod handlers;
mod projection;
mod resolution;
pub mod scopes;
pub use anonymous::run_do_block;
pub use execution::execute_trigger_routine;
pub use handlers::{
    call_bound_user_scalar_function, call_bound_user_scalar_function_with_descriptions,
    call_bound_user_table_function, call_bound_user_table_function_with_descriptions,
    call_user_scalar_function, call_user_table_function, resolved_bound_user_function_returns_set,
    resolved_user_function_returns_set, run_call,
};
pub use projection::{
    call_user_projection_function, has_described_user_function, RoutineProjectionResult,
};

mod plpgsql_cache;
