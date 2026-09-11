//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine callback adapters for the execution crate's query scopes.

mod callbacks;
mod type_resolution;

pub(crate) use callbacks::ScopedEngineHook;
pub(crate) type CteScope = uqa_execution::query::CteScope<crate::session::StatementReadSnapshot>;

mod function_invocation;
