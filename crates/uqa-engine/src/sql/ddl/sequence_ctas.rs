//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence DDL and CREATE TABLE AS execution.

use super::{Engine, SQLError, SQLResult};

pub(in crate::sql) use uqa_execution::schema::ctas::CreateTableAsExecution;

pub(in crate::sql) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.create_sequence_sql(&s)? {
        engine.push_sql_notice(
            "NOTICE",
            &format!("relation \"{}\" already exists, skipping", s.name),
        );
    }
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.alter_sequence_sql(&s)? {
        engine.push_sql_notice(
            "NOTICE",
            &format!("relation \"{}\" does not exist, skipping", s.name),
        );
    }
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_create_table_as(
    engine: &Engine,
    execution: CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_create_table_as_inner(engine, &execution)
    } else {
        engine.transaction(move |engine| run_create_table_as_inner(engine, &execution))
    }
}

fn run_create_table_as_inner(
    engine: &Engine,
    execution: &CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::schema::ctas::run_create_table_as(
        &engine.create_table_as_context(&scope),
        execution,
    )
}
