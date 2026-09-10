//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-scoped query execution and generation-aware result delivery.
mod bound_consumer;
pub mod consumer;
pub mod context;
mod execution;
pub use execution::{
    collect_query_operator, execute_query_plan_output, execute_query_plan_with_ctes,
};

pub mod directional;
pub mod directional_support;
