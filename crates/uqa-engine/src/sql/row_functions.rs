//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-emitting SQL function dispatch and retrieval helpers.

use std::collections::BTreeMap;

use uqa_core::{DocId, Value};
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_operators::OperatorTree;
use uqa_planner::SourcePlan;
use uqa_sql::expr::value_to_vector;
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{SQLError, SQLParam};

use crate::{Engine, ScoredEntry};

mod arguments;
mod dispatch;
mod graph;
mod multi_field;
mod retrieval;
mod validation;

pub(super) use arguments::expect_column_name;
pub(super) use dispatch::{execute_function, execute_function_with_top_k};
pub(super) use graph::{
    execute_tree_entries, expect_optional_graph_value, graph_betweenness_entries,
    graph_hits_entries, graph_pagerank_entries, run_age_create_graph_with_evaluator,
    run_age_drop_graph_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};
pub(crate) use retrieval::{
    run_bayesian_match_with_prior_in_execution, run_bayesian_match_with_prior_public,
    run_calibrated_vector_match_public, run_multi_field_match_in_execution,
    run_multi_field_match_public,
};
pub(super) use validation::{
    validate_expr_text_match_fields, validate_joined_expr_text_match_fields,
};

use arguments::{
    expect_evaluated_string, expect_field_name_or_string, expect_string, expect_usize,
};
use dispatch::RetrievalExecution;
use graph::{run_graph_create, run_graph_drop};
use multi_field::{
    expect_f64_value, multi_field_match_shape, run_multi_field_match, MultiFieldMatchShape,
};
use validation::{validate_text_match_all_fields, validate_text_match_field};
