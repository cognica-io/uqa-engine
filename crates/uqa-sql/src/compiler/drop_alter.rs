//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP, ALTER TABLE, and RENAME lowering.

use super::routines::compile_drop_function;
use super::{
    compile_column_def, compile_expr, compile_pg_type_name, extract_string, range_var_name,
    render_relation_component, AlterTableAction, AlterTableStmt, DropKind, DropStmt, NodeEnum,
    Result, SQLError, Statement, TableKeyConstraint, TableKeyConstraintKind,
};

pub(super) fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectSchema => DropKind::Schema,
        ObjectType::ObjectFunction => return compile_drop_function(stmt, false),
        ObjectType::ObjectProcedure => return compile_drop_function(stmt, true),
        other => {
            return Err(SQLError::Unsupported(format!(
                "DROP target {other:?} not supported"
            )));
        }
    };
    let mut names = Vec::new();
    for object in &stmt.objects {
        let inner = object
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("DROP contains an empty target".into()))?;
        match inner {
            NodeEnum::List(list) => {
                let parts = list
                    .items
                    .iter()
                    .map(extract_string)
                    .collect::<Result<Vec<_>>>()?;
                if parts.is_empty() {
                    return Err(SQLError::Internal("DROP target has no name".into()));
                }
                if matches!(kind, DropKind::Table | DropKind::View) {
                    if parts.len() > 2 {
                        return Err(SQLError::Unsupported(
                            "cross-database DROP targets are not supported".into(),
                        ));
                    }
                    names.push(
                        parts
                            .iter()
                            .map(|part| render_relation_component(part))
                            .collect::<Vec<_>>()
                            .join("."),
                    );
                } else {
                    names.push(parts.last().cloned().unwrap_or_default());
                }
            }
            NodeEnum::String(s) => names.push(s.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "DROP object node {other:?} not supported"
                )));
            }
        }
    }
    if names.is_empty() {
        return Err(SQLError::Internal("DROP without target name".into()));
    }
    let cascade = matches!(stmt.behavior(), DropBehavior::DropCascade);
    Ok(Statement::Drop(DropStmt {
        kind,
        names,
        if_exists: stmt.missing_ok,
        cascade,
    }))
}

// -------------------------------------------------------------------------
// ALTER TABLE { ADD COLUMN | DROP COLUMN | RENAME COLUMN | RENAME TO }
// -------------------------------------------------------------------------

pub(super) fn compile_alter_table(
    stmt: &pg_query::protobuf::AlterTableStmt,
) -> Result<AlterTableStmt> {
    use pg_query::protobuf::{AlterTableType, DropBehavior};
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without relation".into()))?;
    let if_exists = stmt.missing_ok;
    let cmd = stmt
        .cmds
        .first()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without command".into()))?;
    let inner = cmd
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE command body empty".into()))?;
    let cmd = match inner {
        NodeEnum::AlterTableCmd(c) => c,
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE command {other:?}"
            )));
        }
    };
    let action = match cmd.subtype() {
        AlterTableType::AtAddColumn => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ADD COLUMN without ColumnDef".into()))?;
            let col_def = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ADD COLUMN expected ColumnDef, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AddColumn {
                column: col_def,
                if_not_exists: cmd.missing_ok,
            }
        }
        AlterTableType::AtAddConstraint => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|definition| definition.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ADD CONSTRAINT without Constraint".into()))?;
            let constraint = match def_inner {
                NodeEnum::Constraint(constraint) => constraint,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ADD CONSTRAINT expected Constraint, got {other:?}"
                    )));
                }
            };
            let kind = match constraint.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => TableKeyConstraintKind::PrimaryKey,
                pg_query::protobuf::ConstrType::ConstrUnique => TableKeyConstraintKind::Unique,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "ALTER TABLE ADD CONSTRAINT {other:?} is not supported"
                    )));
                }
            };
            let columns = constraint
                .keys
                .iter()
                .map(extract_string)
                .collect::<Result<Vec<_>>>()?;
            if columns.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "PRIMARY KEY / UNIQUE constraint must name at least one column".into(),
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            for column in &columns {
                if !seen.insert(column.as_str()) {
                    return Err(SQLError::TypeMismatch(format!(
                        "PRIMARY KEY / UNIQUE constraint names column `{column}` more than once"
                    )));
                }
            }
            AlterTableAction::AddKeyConstraint {
                constraint: TableKeyConstraint {
                    name: (!constraint.conname.is_empty()).then(|| constraint.conname.clone()),
                    kind,
                    columns,
                    nulls_not_distinct: constraint.nulls_not_distinct,
                },
            }
        }
        AlterTableType::AtDropColumn => AlterTableAction::DropColumn {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
            cascade: matches!(cmd.behavior(), DropBehavior::DropCascade),
        },
        AlterTableType::AtColumnDefault => {
            if let Some(default) = cmd.def.as_deref() {
                AlterTableAction::SetDefault {
                    name: cmd.name.clone(),
                    default: compile_expr(default)?,
                }
            } else {
                AlterTableAction::DropDefault {
                    name: cmd.name.clone(),
                }
            }
        }
        AlterTableType::AtSetNotNull => AlterTableAction::SetNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtDropNotNull => AlterTableAction::DropNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtAlterColumnType => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ALTER COLUMN TYPE without type".into()))?;
            let ty = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?.ty,
                NodeEnum::TypeName(t) => compile_pg_type_name(t, &cmd.name)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ALTER COLUMN TYPE expected ColumnDef/TypeName, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AlterColumnType {
                name: cmd.name.clone(),
                ty,
            }
        }
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE action {other:?}"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists,
        action,
    })
}

pub(super) fn compile_rename(stmt: &pg_query::protobuf::RenameStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::ObjectType;
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("RENAME without relation".into()))?;
    let action = match stmt.rename_type() {
        ObjectType::ObjectColumn => AlterTableAction::RenameColumn {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectTable => AlterTableAction::RenameTable {
            to: render_relation_component(&stmt.newname),
        },
        other => {
            return Err(SQLError::Unsupported(format!(
                "RENAME target {other:?} not supported"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists: stmt.missing_ok,
        action,
    })
}
