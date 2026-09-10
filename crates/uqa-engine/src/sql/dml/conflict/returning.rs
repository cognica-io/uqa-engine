//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind RETURNING metadata and physical row execution to the active engine generation.
use crate::Engine;
use uqa_execution::RowSchema;
use uqa_sql::{ast::Statement, SQLError, SQLParam};

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
