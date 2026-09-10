//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use super::{CteScope, Engine, InsertPlan, SQLError, SQLParam, SQLResult};

pub(in crate::sql) fn run_insert(
    engine: &Engine,
    mut stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_insert_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_insert_with_ctes(
    engine: &Engine,
    mut stmt: InsertPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_insert_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_insert_inner_with_ctes(engine, stmt, params, None)
}

fn run_insert_inner_with_ctes(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    uqa_execution::mutation::dispatch::run_insert(
        &engine.statement_execution_context(),
        uqa_execution::mutation::insert::table::InsertPlanning {
            inference: engine.inference_context(),
            returning: engine.returning_analysis_context(),
            prune_source_outputs: uqa_planner::mutation_outputs::prune_unused_query_outputs,
        },
        stmt,
        params,
        inherited_ctes,
    )
}
