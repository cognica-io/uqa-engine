//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE candidate selection, command policy, staging, and publication.

use super::{
    validate_returning_alias_relations, CteScope, DeletePlan, Engine, SQLError, SQLParam, SQLResult,
};

pub(in crate::sql) fn run_delete(
    engine: &Engine,
    mut stmt: DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    super::run_mutation_command(engine, move |engine| {
        run_delete_inner(engine, &stmt, params)
    })
}

pub(in crate::sql) fn run_delete_with_ctes(
    engine: &Engine,
    mut stmt: DeletePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.table = super::resolve_dml_target_name(engine, &stmt.table, stmt.target_relation_bound)?;
    super::run_mutation_command(engine, move |engine| {
        run_delete_inner_with_ctes(engine, &stmt, params, Some(ctes))
    })
}

pub(in crate::sql) fn run_delete_inner(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    run_delete_inner_with_ctes(engine, stmt, params, None)
}

fn run_delete_inner_with_ctes(
    engine: &Engine,
    stmt: &DeletePlan,
    params: &[SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<SQLResult, SQLError> {
    uqa_execution::mutation::dispatch::run_delete(
        &engine.mutation_statement_context(),
        uqa_planner::mutation_outputs::prune_unused_query_outputs,
        stmt,
        params,
        inherited_ctes,
    )
}
