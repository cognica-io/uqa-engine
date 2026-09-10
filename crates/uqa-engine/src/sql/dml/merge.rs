//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE matching, action execution, and RETURNING projection.

use super::{CteScope, Engine, MergePlan, SQLError, SQLParam, SQLResult};
pub(in crate::sql) fn merge_command_returning_schema(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    uqa_execution::mutation::merge::analysis::merge_command_returning_schema(
        &engine.returning_execution_context(),
        engine,
        stmt,
        params,
    )
}

pub(in crate::sql) fn run_merge(
    engine: &Engine,
    mut stmt: MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.target = super::resolve_dml_target_name(engine, &stmt.target, false)?;
    super::run_mutation_command(engine, move |engine| {
        uqa_execution::mutation::dispatch::run_merge(
            &engine.statement_execution_context(),
            uqa_planner::mutation_outputs::prune_unused_query_outputs,
            &stmt,
            params,
            None,
        )
    })
}

pub(in crate::sql) fn run_merge_with_ctes(
    engine: &Engine,
    mut stmt: MergePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SQLResult, SQLError> {
    stmt.target = super::resolve_dml_target_name(engine, &stmt.target, false)?;
    super::run_mutation_command(engine, move |engine| {
        uqa_execution::mutation::dispatch::run_merge(
            &engine.statement_execution_context(),
            uqa_planner::mutation_outputs::prune_unused_query_outputs,
            &stmt,
            params,
            Some(ctes),
        )
    })
}
