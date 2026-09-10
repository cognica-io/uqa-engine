//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_execution::{eval_call_arguments, ScalarEvalContext, ScalarExpr};
use uqa_planner::QueryPlan;
use uqa_sql::{SQLError, SQLParam};

use crate::Engine;

use super::scalar::{PhysicalSubqueryRunner, PlanSubqueryArena};
use super::select::{CteScope, QueryOutput};
use super::{
    age_cypher, doc_id_value, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, json_table_arg, json_table_value_to_text,
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};

use uqa_sql::semantics::source_filters::checked_integer_value;

/// Pull-based local-table source used by join leaves. It advances through the
/// document store with `next_doc_id`, so neither ids nor documents are copied
/// into a cardinality-sized staging vector before the physical join sees its
/// first batch.
mod functions;
mod lateral;
mod source_qualification;
mod table_function_core;
mod table_function_dispatch;
mod table_function_values;

pub(in crate::sql) use functions::*;
pub(in crate::sql) use lateral::*;
pub(in crate::sql) use source_qualification::*;
pub(in crate::sql) use table_function_core::*;
pub(in crate::sql) use table_function_dispatch::*;
pub(in crate::sql) use table_function_values::*;

#[cfg(test)]
mod tests;
