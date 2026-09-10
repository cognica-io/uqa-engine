//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind foreign-key declaration analysis and row validation to the active catalog and generation.
use crate::Engine;
pub(in crate::sql::ddl) use uqa_sql::schema::foreign_keys::column_foreign_key;
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableKeyConstraint},
    SQLError,
};
pub(super) fn validate_foreign_key_definition(
    engine: &Engine,
    table: &str,
    foreign_key: &mut ForeignKey,
) -> Result<(), SQLError> {
    uqa_sql::schema::foreign_keys::validate_foreign_key_definition(
        &engine.foreign_key_definition_context(),
        table,
        foreign_key,
    )
}
pub(in crate::sql::ddl) fn validate_foreign_key_definition_with_local_state(
    engine: &Engine,
    table: &str,
    columns: Option<&[ColumnDef]>,
    keys: Option<&[TableKeyConstraint]>,
    foreign_key: &mut ForeignKey,
) -> Result<(), SQLError> {
    uqa_sql::schema::foreign_keys::validate_foreign_key_definition_with_local_state(
        &engine.foreign_key_definition_context(),
        table,
        columns,
        keys,
        foreign_key,
    )
}
pub(super) fn validate_foreign_key_rows(
    engine: &Engine,
    table: &str,
    name: &str,
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    uqa_execution::schema::validation::validate_foreign_key_rows(
        engine.constraint_execution_context(),
        table,
        name,
        foreign_key,
    )
}
