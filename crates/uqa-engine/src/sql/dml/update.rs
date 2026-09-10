//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE execution, point-update fast paths, and patch eligibility.

use super::{CteScope, Engine, SQLError, SQLParam, SQLResult, UpdatePlan};

pub(in crate::sql) fn run_update(
    engine: &Engine,
    mut stmt: UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_update_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_update_with_ctes(
    engine: &Engine,
    mut stmt: UpdatePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_update_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_update_inner(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_update_inner_with_ctes(engine, stmt, params, None)
}

fn run_update_inner_with_ctes(
    engine: &Engine,
    stmt: &UpdatePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    uqa_execution::mutation::dispatch::run_update(
        &engine.statement_execution_context(),
        uqa_planner::mutation_outputs::prune_unused_query_outputs,
        stmt,
        params,
        inherited_ctes,
    )
}
