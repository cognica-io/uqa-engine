//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined function and procedure coverage: `CREATE FUNCTION` /
//! `CREATE PROCEDURE` / `DO` / `CALL` with `LANGUAGE plpgsql` and
//! `LANGUAGE sql`. Expected outcomes were verified against
//! `PostgreSQL` 17.7 unless a comment states a documented divergence.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::SQLError;

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    match engine.sql(sql, &[]) {
        Ok(result) => result,
        Err(e) => panic!("SQL failed: {e}\n  sql: {sql}"),
    }
}

fn exec_err(engine: &Engine, sql: &str) -> SQLError {
    match engine.sql(sql, &[]) {
        Ok(_) => panic!("expected error for: {sql}"),
        Err(e) => e,
    }
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = exec(engine, sql);
    let row = result.rows.first().unwrap_or_else(|| {
        panic!("no rows for: {sql}");
    });
    let column = result.columns.first().expect("no columns");
    row.get(column).cloned().unwrap_or(Value::Null)
}

fn engine() -> Engine {
    Engine::new()
}

#[path = "sql_plpgsql/catalog_lifecycle.rs"]
mod catalog_lifecycle;
#[path = "sql_plpgsql/control_flow.rs"]
mod control_flow;
#[path = "sql_plpgsql/diagnostics.rs"]
mod diagnostics;
#[path = "sql_plpgsql/dynamic_recursion.rs"]
mod dynamic_recursion;
#[path = "sql_plpgsql/exceptions.rs"]
mod exceptions;
#[path = "sql_plpgsql/misc_semantics.rs"]
mod misc_semantics;
#[path = "sql_plpgsql/procedures.rs"]
mod procedures;
#[path = "sql_plpgsql/scalar_and_resolution.rs"]
mod scalar_and_resolution;
#[path = "sql_plpgsql/set_returning.rs"]
mod set_returning;
#[path = "sql_plpgsql/sql_language.rs"]
mod sql_language;
