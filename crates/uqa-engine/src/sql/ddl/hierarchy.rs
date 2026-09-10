//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply catalog metadata and partition binding for CREATE TABLE inheritance.
use crate::Engine;
use uqa_sql::{ast::CreateTable, SQLError};
pub(super) fn prepare_create_table_hierarchy(
    engine: &Engine,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    uqa_sql::schema::inheritance::prepare_create_table_hierarchy(
        &uqa_sql::schema::inheritance::InheritanceContext {
            catalog: engine,
            partitions: engine.partition_context(),
            roles: engine,
        },
        table,
    )
}
