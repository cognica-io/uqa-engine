//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct relation deletion shares SQL binding, authorization and dependency preflight.

use super::{binding, run_drop_inner, RelationRemovalContext};
use uqa_sql::{
    ast::{DropKind, DropStmt},
    SQLError,
};

/// The caller supplies a definition transaction, including for an in-memory engine.
pub fn drop_table(context: &RelationRemovalContext<'_>, name: &str) -> Result<bool, SQLError> {
    drop_relation(context, name, DropKind::Table)
}

/// The caller supplies a definition transaction, including for an in-memory engine.
pub fn drop_foreign_table(
    context: &RelationRemovalContext<'_>,
    name: &str,
) -> Result<bool, SQLError> {
    drop_relation(context, name, DropKind::ForeignTable)
}

fn drop_relation(
    context: &RelationRemovalContext<'_>,
    name: &str,
    kind: DropKind,
) -> Result<bool, SQLError> {
    let statement = DropStmt {
        kind,
        names: vec![name.to_string()],
        if_exists: true,
        cascade: false,
    };
    // Direct APIs report absence through their boolean result instead of SQL notices.
    let names = binding::bind_drop_targets(context, &statement, &mut |_| {})?;
    if names.is_empty() {
        return Ok(false);
    }
    run_drop_inner(context, DropStmt { names, ..statement })?;
    Ok(true)
}
