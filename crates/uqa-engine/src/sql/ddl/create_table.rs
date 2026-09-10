//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind CREATE TABLE execution to the Engine transaction scope.
use crate::Engine;
use uqa_sql::{
    ast::{CreateTable, DeferredCreateTable},
    SQLError, SQLResult,
};
pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    table: CreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| {
        uqa_execution::schema::table_creation::run_create_table(
            &engine.create_table_context(),
            table,
        )
    })
}
pub(in crate::sql) fn run_create_table_if_not_exists(
    engine: &Engine,
    deferred: DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| {
        uqa_execution::schema::table_creation::run_create_table_if_not_exists(
            &engine.create_table_context(),
            deferred,
        )
    })
}
