//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lower relation namespace moves to their owning lifecycle statements.

use super::{range_var_name, render_relation_component, Result, SQLError};
use crate::ast::{
    AlterForeignTableAction, AlterForeignTableStmt, AlterSequence, AlterTableAction,
    AlterTableStmt, AlterViewAction, AlterViewKind, AlterViewStmt, SequenceLifecycle, Statement,
};

pub(super) fn compile_alter_object_schema(
    stmt: &pg_query::protobuf::AlterObjectSchemaStmt,
) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER SET SCHEMA without relation".into()))?;
    let name = range_var_name(relation);
    let schema = render_relation_component(&stmt.newschema);
    match stmt.object_type() {
        ObjectType::ObjectSequence => Ok(Statement::AlterSequence(AlterSequence {
            name,
            if_exists: stmt.missing_ok,
            lifecycle: SequenceLifecycle::SetSchema { schema },
            ..AlterSequence::default()
        })),
        ObjectType::ObjectTable => Ok(Statement::AlterTable(AlterTableStmt {
            table: name,
            qualifier: relation.relname.clone(),
            if_exists: stmt.missing_ok,
            recurse: false,
            actions: vec![AlterTableAction::SetSchema { schema }],
        })),
        ObjectType::ObjectView | ObjectType::ObjectMatview => {
            Ok(Statement::AlterView(AlterViewStmt {
                name,
                kind: if stmt.object_type() == ObjectType::ObjectView {
                    AlterViewKind::View
                } else {
                    AlterViewKind::MaterializedView
                },
                if_exists: stmt.missing_ok,
                action: AlterViewAction::SetSchema(schema),
            }))
        }
        ObjectType::ObjectForeignTable => Ok(Statement::AlterForeignTable(AlterForeignTableStmt {
            name,
            if_exists: stmt.missing_ok,
            action: AlterForeignTableAction::SetSchema(schema),
        })),
        other => Err(SQLError::Unsupported(format!(
            "ALTER {other:?} SET SCHEMA is not supported"
        ))),
    }
}
