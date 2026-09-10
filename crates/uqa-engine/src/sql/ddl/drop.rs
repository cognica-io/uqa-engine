//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind relation removal and enter namespace lifecycle operations.
use super::{DropKind, DropStmt, Engine, SQLError, SQLResult};

pub(in crate::sql) fn run_drop(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    if stmt.kind == DropKind::Schema {
        return engine.with_implicit_transaction(|engine| {
            engine.drop_schemas_sql(&stmt)?;
            Ok(SQLResult::empty())
        });
    }
    if stmt.kind == DropKind::Domain {
        return engine.with_implicit_transaction(|engine| {
            engine.drop_domains_sql(&stmt)?;
            Ok(SQLResult::empty())
        });
    }
    uqa_execution::schema::removal::run_drop(&engine.relation_removal_context(), stmt)
}
