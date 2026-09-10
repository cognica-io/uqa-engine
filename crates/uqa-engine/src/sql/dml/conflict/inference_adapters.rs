//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind inference analysis to the current catalog and stored-expression namespace.
use crate::Engine;
use uqa_sql::{
    plan::{ConflictPlan, InsertPlan},
    SQLError, SQLParam,
};

pub(in crate::sql) fn prepare_inference_predicate<'a>(
    engine: &Engine,
    statement: &'a InsertPlan,
    params: &[SQLParam],
) -> Result<std::borrow::Cow<'a, InsertPlan>, SQLError> {
    uqa_sql::semantics::conflict::prepare_inference_predicate(
        engine.inference_context(),
        statement,
        params,
    )
}

pub(in crate::sql) fn validate_conflict_target(
    engine: &Engine,
    table: &str,
    conflict: &ConflictPlan,
) -> Result<(), SQLError> {
    uqa_sql::semantics::conflict::validate_conflict_target(engine, table, conflict)
}
