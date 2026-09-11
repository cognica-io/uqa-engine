//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_planner::QueryPlan;
use uqa_sql::{SQLError, SQLParam};

use crate::Engine;

use super::select::{CteScope, QueryOutput};
use super::{
    expect_column_name, run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};

use uqa_sql::semantics::source_filters::checked_integer_value;

mod functions;
mod lateral;

pub(in crate::sql) use functions::*;
pub(in crate::sql) use lateral::*;

#[cfg(test)]
mod tests;
