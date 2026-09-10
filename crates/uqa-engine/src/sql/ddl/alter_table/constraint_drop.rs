//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind dependent-object removal to the current execution contexts.
use crate::Engine;
use uqa_sql::SQLError;
pub(crate) fn drop_constraint_dependency(
    engine: &Engine,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::drop::drop_constraint_dependency(
        &engine.constraint_alter_context(),
        table,
        name,
    )
}
pub(crate) fn drop_column_cascade(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    uqa_execution::schema::columns::removal::drop_column_cascade(
        &engine.column_removal_context(),
        table,
        column,
        if_exists,
    )
}
