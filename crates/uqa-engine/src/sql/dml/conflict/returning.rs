//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind RETURNING metadata and physical row execution to the active engine generation.
use crate::sql::CteScope;
use crate::Engine;
pub(in crate::sql) use uqa_execution::mutation::returning::ReturningValueProjectionRow;
use uqa_execution::{OwnedPhysicalRow, RowSchema};
pub(in crate::sql) use uqa_sql::semantics::returning::document_supplied_id;
pub(in crate::sql) use uqa_sql::semantics::returning_expression_schema;
use uqa_sql::{
    ast::{ReturningAliases, Statement},
    plan::ProjectionPlan,
    SQLError, SQLParam, SQLResult,
};
pub(in crate::sql) type DmlReturningShape<'a> =
    uqa_execution::mutation::returning::DmlReturningShape<
        'a,
        crate::session::StatementReadSnapshot,
    >;

pub(in crate::sql) fn build_returning_value_row(
    engine: &Engine,
    input: ReturningValueProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    uqa_execution::mutation::returning::build_returning_value_row(
        engine.returning_execution_context(),
        input,
        returning,
        params,
        ctes,
    )
}

pub(in crate::sql) fn returning_value_context(
    engine: &Engine,
    input: ReturningValueProjectionRow<'_>,
) -> Result<OwnedPhysicalRow, SQLError> {
    uqa_execution::mutation::returning::returning_value_context(
        engine.returning_execution_context(),
        input,
    )
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    uqa_execution::mutation::returning::dml_returning_result(
        engine.returning_execution_context(),
        shape,
        rows,
        affected_rows,
    )
}

pub(in crate::sql) fn dml_returning_result_with_projections(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    projections: &[ProjectionPlan],
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    uqa_execution::mutation::returning::dml_returning_result_with_projections(
        engine.returning_execution_context(),
        shape,
        projections,
        rows,
        affected_rows,
    )
}

pub(in crate::sql) fn returning_target_schema(
    engine: &Engine,
    table: &str,
) -> Result<RowSchema, SQLError> {
    uqa_sql::semantics::returning::returning_target_schema(engine, table)
}

pub(in crate::sql) fn expanded_returning_projections(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    uqa_sql::semantics::returning::expanded_returning_projections(
        engine,
        table,
        target_qualifier,
        aliases,
        returning,
    )
}

pub(in crate::sql) fn dml_statement_returning_schema(
    engine: &Engine,
    statement: Statement,
) -> Result<Option<RowSchema>, SQLError> {
    uqa_sql::semantics::returning::dml_statement_returning_schema(
        engine.returning_analysis_context(),
        statement,
    )
}

pub(in crate::sql) fn dml_command_returning_schema(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    uqa_sql::semantics::returning::dml_command_returning_schema(
        engine.returning_analysis_context(),
        command,
        params,
    )
}

pub(in crate::sql) fn validate_insert_returning(
    engine: &Engine,
    plan: &uqa_planner::InsertPlan,
    params: &[SQLParam],
    inherited: Option<&CteScope>,
) -> Result<(), SQLError> {
    uqa_sql::semantics::returning::validate_insert_returning(
        engine.returning_analysis_context(),
        plan,
        params,
        inherited.map(|scope| scope as &dyn uqa_sql::semantics::returning::ReturningScope),
    )
}
