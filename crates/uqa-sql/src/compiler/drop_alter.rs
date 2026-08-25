//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP, ALTER TABLE, and RENAME lowering.

use super::routines::compile_drop_function;
use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, validate_foreign_key_set_columns,
};
use super::{
    compile_column_def, compile_expr, compile_pg_type_name, extract_string, range_var_name,
    render_relation_component, AlterTableAction, AlterTableStmt, DropKind, DropStmt, NodeEnum,
    Result, SQLError, Statement, TableKeyConstraint, TableKeyConstraintKind,
};
use crate::ast::{ForeignKey, TableCheck};

fn extract_strings(nodes: &[pg_query::protobuf::Node]) -> Result<Vec<String>> {
    nodes.iter().map(extract_string).collect()
}

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
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without relation".into()))?;
    let table = range_var_name(relation);
    let qualifier = relation.relname.clone();
    let if_exists = stmt.missing_ok;
    if stmt.cmds.is_empty() {
        return Err(SQLError::Internal("ALTER TABLE without command".into()));
    }
    let mut actions = Vec::with_capacity(stmt.cmds.len());
    for command in &stmt.cmds {
        let inner = command
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
                    .ok_or_else(|| {
                        SQLError::Internal("ADD CONSTRAINT without Constraint".into())
                    })?;
                let constraint = match def_inner {
                    NodeEnum::Constraint(constraint) => constraint,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "ADD CONSTRAINT expected Constraint, got {other:?}"
                        )));
                    }
                };
                let name = (!constraint.conname.is_empty()).then(|| constraint.conname.clone());
                match constraint.contype() {
                    pg_query::protobuf::ConstrType::ConstrPrimary
                    | pg_query::protobuf::ConstrType::ConstrUnique => {
                        let kind = if constraint.contype()
                            == pg_query::protobuf::ConstrType::ConstrPrimary
                        {
                            TableKeyConstraintKind::PrimaryKey
                        } else {
                            TableKeyConstraintKind::Unique
                        };
                        let columns = extract_strings(&constraint.keys)?;
                        if columns.is_empty() {
                            return Err(SQLError::TypeMismatch(
                                "PRIMARY KEY / UNIQUE constraint must name at least one column"
                                    .into(),
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
                                name,
                                kind,
                                columns,
                                nulls_not_distinct: constraint.nulls_not_distinct,
                                without_overlaps: constraint.without_overlaps,
                            },
                        }
                    }
                    pg_query::protobuf::ConstrType::ConstrCheck => {
                        let raw = constraint.raw_expr.as_deref().ok_or_else(|| {
                            SQLError::Internal("ADD CHECK without expression".into())
                        })?;
                        AlterTableAction::AddCheckConstraint {
                            constraint: TableCheck {
                                name,
                                expr: compile_expr(raw)?,
                                enforced: constraint.is_enforced,
                                validated: constraint.initially_valid,
                                no_inherit: constraint.is_no_inherit,
                            },
                        }
                    }
                    pg_query::protobuf::ConstrType::ConstrForeign => {
                        if constraint.fk_with_period != constraint.pk_with_period {
                            return Err(SQLError::TypeMismatch(
                                "FOREIGN KEY must use PERIOD on both the referencing and referenced key"
                                    .into(),
                            ));
                        }
                        let local_columns = extract_strings(&constraint.fk_attrs)?;
                        let ref_table = constraint
                            .pktable
                            .as_ref()
                            .map(range_var_name)
                            .ok_or_else(|| {
                                SQLError::Internal("FOREIGN KEY without referenced table".into())
                            })?;
                        let ref_columns = extract_strings(&constraint.pk_attrs)?;
                        if local_columns.is_empty()
                            || !ref_columns.is_empty() && local_columns.len() != ref_columns.len()
                        {
                            return Err(SQLError::TypeMismatch(
                                "FOREIGN KEY local and referenced column counts must match".into(),
                            ));
                        }
                        let on_delete_set_columns = extract_strings(&constraint.fk_del_set_cols)?;
                        validate_foreign_key_set_columns(
                            &local_columns,
                            &on_delete_set_columns,
                            &constraint.fk_del_action,
                        )?;
                        let foreign_key = ForeignKey {
                            name,
                            local_columns,
                            ref_table,
                            ref_columns,
                            on_update: compile_foreign_key_action(&constraint.fk_upd_action)?,
                            on_delete: compile_foreign_key_action(&constraint.fk_del_action)?,
                            on_delete_set_columns,
                            match_type: compile_foreign_key_match(&constraint.fk_matchtype)?,
                            enforced: constraint.is_enforced,
                            validated: constraint.initially_valid,
                            deferrable: constraint.deferrable,
                            initially_deferred: constraint.initdeferred,
                            period: constraint.fk_with_period,
                        };
                        if foreign_key.period
                            && (!matches!(
                                foreign_key.on_update,
                                crate::ast::ForeignKeyAction::NoAction
                            ) || !matches!(
                                foreign_key.on_delete,
                                crate::ast::ForeignKeyAction::NoAction
                            ))
                        {
                            return Err(SQLError::Unsupported(
                                "unsupported referential action for foreign key constraint using PERIOD"
                                    .into(),
                            ));
                        }
                        AlterTableAction::AddForeignKeyConstraint {
                            constraint: foreign_key,
                        }
                    }
                    pg_query::protobuf::ConstrType::ConstrNotnull => {
                        let columns = extract_strings(&constraint.keys)?;
                        let [column] = columns.as_slice() else {
                            return Err(SQLError::TypeMismatch(
                                "NOT NULL constraint must name exactly one column".into(),
                            ));
                        };
                        AlterTableAction::AddNotNullConstraint {
                            name,
                            column: column.clone(),
                            validated: constraint.initially_valid,
                            no_inherit: constraint.is_no_inherit,
                        }
                    }
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "ALTER TABLE ADD CONSTRAINT {other:?} is not supported"
                        )));
                    }
                }
            }
            AlterTableType::AtValidateConstraint => AlterTableAction::ValidateConstraint {
                name: cmd.name.clone(),
            },
            AlterTableType::AtAlterConstraint => {
                let definition = cmd
                    .def
                    .as_ref()
                    .and_then(|definition| definition.node.as_ref())
                    .ok_or_else(|| {
                        SQLError::Internal("ALTER CONSTRAINT without definition".into())
                    })?;
                let NodeEnum::AtalterConstraint(definition) = definition else {
                    return Err(SQLError::Internal(format!(
                        "ALTER CONSTRAINT expected ATAlterConstraint, got {definition:?}"
                    )));
                };
                AlterTableAction::AlterConstraint {
                    name: definition.conname.clone(),
                    enforceability: definition
                        .alter_enforceability
                        .then_some(definition.is_enforced),
                    deferrability: definition
                        .alter_deferrability
                        .then_some((definition.deferrable, definition.initdeferred)),
                    no_inherit: definition
                        .alter_inheritability
                        .then_some(definition.noinherit),
                }
            }
            AlterTableType::AtDropConstraint => AlterTableAction::DropConstraint {
                name: cmd.name.clone(),
                if_exists: cmd.missing_ok,
                cascade: matches!(cmd.behavior(), DropBehavior::DropCascade),
            },
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
            AlterTableType::AtSetExpression => {
                let expression = cmd.def.as_deref().ok_or_else(|| {
                    SQLError::Internal("SET EXPRESSION without expression".into())
                })?;
                AlterTableAction::SetExpression {
                    name: cmd.name.clone(),
                    expression: compile_expr(expression)?,
                }
            }
            AlterTableType::AtDropExpression => AlterTableAction::DropExpression {
                name: cmd.name.clone(),
            },
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
                let (ty, using) = match def_inner {
                    NodeEnum::ColumnDef(column) => (
                        compile_column_def(column)?.ty,
                        column
                            .raw_default
                            .as_deref()
                            .map(compile_expr)
                            .transpose()?,
                    ),
                    NodeEnum::TypeName(type_name) => {
                        (compile_pg_type_name(type_name, &cmd.name)?, None)
                    }
                    other => {
                        return Err(SQLError::Internal(format!(
                            "ALTER COLUMN TYPE expected ColumnDef/TypeName, got {other:?}"
                        )));
                    }
                };
                AlterTableAction::AlterColumnType {
                    name: cmd.name.clone(),
                    ty,
                    using,
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE action {other:?}"
                )));
            }
        };
        actions.push(action);
    }
    Ok(AlterTableStmt {
        table,
        qualifier,
        if_exists,
        actions,
    })
}

pub(super) fn compile_rename(stmt: &pg_query::protobuf::RenameStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::ObjectType;
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("RENAME without relation".into()))?;
    let table = range_var_name(relation);
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
        qualifier: relation.relname.clone(),
        if_exists: stmt.missing_ok,
        actions: vec![action],
    })
}
