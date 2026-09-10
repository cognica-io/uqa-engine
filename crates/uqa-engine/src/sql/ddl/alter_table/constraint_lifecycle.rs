//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind remaining ALTER dispatch to constraint execution and SQL declaration analysis.
use crate::Engine;
pub(super) use uqa_sql::schema::constraint_changes::{
    constraint_error, ensure_constraint_name_available, ensure_not_null_inheritable,
};
use uqa_sql::{ast::ForeignKey, SQLError};

pub(super) fn table_constraint_state(
    engine: &Engine,
    table: &str,
) -> Result<
    (
        Vec<uqa_sql::ast::ColumnDef>,
        uqa_sql::ast::TableConstraintSet,
    ),
    SQLError,
> {
    uqa_execution::schema::constraints::table_constraint_state(
        &engine.constraint_alter_context(),
        table,
    )
}

pub(super) fn add_check_constraint(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    constraint: uqa_sql::ast::TableCheck,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::add_check_constraint(
        &engine.constraint_alter_context(),
        table,
        qualifier,
        constraint,
    )
}

pub(super) fn add_foreign_key_constraint(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    constraint: uqa_sql::ast::ForeignKey,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::add_foreign_key_constraint(
        &engine.constraint_alter_context(),
        table,
        qualifier,
        constraint,
    )
}

pub(super) fn set_not_null_constraint(
    engine: &Engine,
    table: &str,
    column: &str,
    recurse: bool,
    is_local: bool,
    inherited_name: Option<String>,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::set_not_null_constraint(
        &engine.constraint_alter_context(),
        table,
        column,
        recurse,
        is_local,
        inherited_name,
    )
}

pub(super) fn add_not_null_constraint(
    engine: &Engine,
    table: &str,
    name: Option<String>,
    column: &str,
    validated: bool,
    no_inherit: bool,
    is_local: bool,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::add_not_null_constraint(
        &engine.constraint_alter_context(),
        table,
        name,
        column,
        validated,
        no_inherit,
        is_local,
    )
}

pub(super) fn validate_and_mark_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::validate_and_mark_constraint(
        &engine.constraint_alter_context(),
        table,
        name,
    )
}

pub(super) fn validate_not_null_rows(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::validate_not_null_rows(
        &engine.constraint_alter_context(),
        table,
        column,
    )
}

pub(super) fn alter_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
    enforceability: Option<bool>,
    deferrability: Option<(bool, bool)>,
    no_inherit: Option<bool>,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::alter_constraint(
        &engine.constraint_alter_context(),
        table,
        name,
        enforceability,
        deferrability,
        no_inherit,
    )
}

pub(super) fn validate_altered_constraint_column_types(
    engine: &Engine,
    table: &str,
    candidate_columns: &[uqa_sql::ast::ColumnDef],
    key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    uqa_sql::schema::constraint_changes::validate_altered_constraint_column_types(
        &engine.constraint_type_context(),
        table,
        candidate_columns,
        key_constraints,
        foreign_keys,
    )
}
