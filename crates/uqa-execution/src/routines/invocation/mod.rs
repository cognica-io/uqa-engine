//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine invocation, scoped state, interpreter entry, and physical result shaping.
mod anonymous;
pub mod context;
mod depth;
mod execution;
mod handlers;
mod resolution;
pub mod scopes;
pub use anonymous::run_do_block;
pub use execution::execute_trigger_routine;
pub use handlers::{
    call_bound_user_scalar_function, call_bound_user_table_function, call_user_scalar_function,
    call_user_table_function, resolved_bound_user_function_returns_set,
    resolved_user_function_returns_set, run_call,
};
