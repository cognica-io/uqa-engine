//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-emitting SQL function dispatch and retrieval helpers.

use uqa_core::Value;
use uqa_execution::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{SQLError, SQLParam};

use crate::{Engine, ScoredEntry};

mod dispatch;
mod graph;

pub(super) use graph::{
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};

use graph::{run_graph_create, run_graph_drop};
use uqa_sql::semantics::retrieval::expect_evaluated_string;
