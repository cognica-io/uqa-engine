//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine callback adapters for the execution crate's query scopes.

mod callbacks;
mod subqueries;
mod type_resolution;

pub(crate) use callbacks::{prepare_correlated_exists_predicate, ScopedEngineHook};
pub(in crate::sql) use uqa_execution::query::scope::expr_contains_subquery;
pub(crate) type CteScope = uqa_execution::query::CteScope<crate::session::StatementReadSnapshot>;

mod function_invocation;
