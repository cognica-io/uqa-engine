//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind index creation to the active catalog and physical storage generation.
use crate::Engine;
use uqa_sql::{ast::CreateIndex, SQLError, SQLResult};
pub(in crate::sql) fn run_create_index(
    engine: &Engine,
    statement: CreateIndex,
) -> Result<SQLResult, SQLError> {
    uqa_execution::schema::indexes::creation::run_create_index(
        &engine.index_creation_context(),
        statement,
    )
}
