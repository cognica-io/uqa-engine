//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind recursive ALTER dispatch to CHECK execution.
use crate::Engine;
pub(super) use uqa_sql::schema::constraint_changes::take_column_check;
use uqa_sql::{ast::TableCheck, SQLError};

pub(super) fn validate_check(
    engine: &Engine,
    table: &str,
    name: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    uqa_execution::schema::constraints::checks::validate_check(
        &engine.constraint_alter_context(),
        table,
        name,
        recurse,
    )
}

pub(super) fn merge_added_check(
    engine: &Engine,
    table: &str,
    incoming: TableCheck,
) -> Result<bool, SQLError> {
    uqa_execution::schema::constraints::checks::merge_added_check(
        &engine.constraint_alter_context(),
        table,
        incoming,
    )
}

pub(super) fn rename_check(
    engine: &Engine,
    table: &str,
    from: &str,
    to: &str,
    recurse: bool,
) -> Result<bool, SQLError> {
    uqa_execution::schema::constraints::checks::rename_check(
        &engine.constraint_alter_context(),
        table,
        from,
        to,
        recurse,
    )
}
