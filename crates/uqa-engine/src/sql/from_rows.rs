//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

#[cfg(test)]
use uqa_core::Value;
#[cfg(test)]
use uqa_execution::ScalarExpr;
use uqa_planner::QueryPlan;
use uqa_sql::{SQLError, SQLParam};

use crate::Engine;

use super::select::{CteScope, QueryOutput};

mod lateral;

pub(in crate::sql) use lateral::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
use uqa_execution::query::scalar_functions::intercept_function as engine_func_intercept;
