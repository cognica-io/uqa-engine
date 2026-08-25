//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP, ALTER TABLE, and RENAME lowering.

use super::relations::{
    collect_def_elem_options, validate_materialized_view_options, validate_view_options,
};
use super::routines::compile_drop_function;
use super::{
    compile_column_def, compile_expr, compile_pg_type_name, extract_string, range_var_name,
    render_relation_component, AlterTableAction, AlterTableStmt, AlterViewKind,
    AlterViewOptionsAction, AlterViewOptionsStmt, DropKind, DropStmt, Node, NodeEnum, Result,
    SQLError, Statement, TableKeyConstraint, TableKeyConstraintKind,
};

pub(super) fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectMatview => DropKind::MaterializedView,
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
                if matches!(
                    kind,
                    DropKind::Table | DropKind::View | DropKind::MaterializedView
                ) {
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

pub(super) fn compile_alter_table(stmt: &pg_query::protobuf::AlterTableStmt) -> Result<Statement> {
    use pg_query::protobuf::{AlterTableType, DropBehavior, ObjectType};
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without relation".into()))?;
    let table = range_var_name(relation);
    let qualifier = relation.relname.clone();
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
    if matches!(
        stmt.objtype(),
        ObjectType::ObjectView | ObjectType::ObjectMatview
    ) {
        let kind = match stmt.objtype() {
            ObjectType::ObjectView => AlterViewKind::View,
            ObjectType::ObjectMatview => AlterViewKind::MaterializedView,
            _ => unreachable!("view kinds were checked above"),
        };
        let action = match cmd.subtype() {
            AlterTableType::AtSetRelOptions => {
                let nodes = alter_reloption_nodes(cmd)?;
                let options = collect_def_elem_options(nodes)?;
                let options = match kind {
                    AlterViewKind::View => validate_view_options(
                        options,
                        pg_query::protobuf::ViewCheckOption::NoCheckOption,
                    )?,
                    AlterViewKind::MaterializedView => validate_materialized_view_options(options)?,
                };
                AlterViewOptionsAction::Set(options)
            }
            AlterTableType::AtResetRelOptions => {
                let nodes = alter_reloption_nodes(cmd)?;
                let names = collect_reset_reloption_names(nodes, kind)?;
                AlterViewOptionsAction::Reset(names)
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER {} action {other:?} is not supported",
                    match kind {
                        AlterViewKind::View => "VIEW",
                        AlterViewKind::MaterializedView => "MATERIALIZED VIEW",
                    }
                )));
            }
        };
        return Ok(Statement::AlterViewOptions(AlterViewOptionsStmt {
            name: table,
            kind,
            if_exists,
            action,
        }));
    }
    if stmt.objtype() != ObjectType::ObjectTable {
        return Err(SQLError::Unsupported(format!(
            "ALTER target {:?} is not supported",
            stmt.objtype()
        )));
    }
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
        AlterTableType::AtSetExpression => {
            let expression = cmd
                .def
                .as_deref()
                .ok_or_else(|| SQLError::Internal("SET EXPRESSION without expression".into()))?;
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
    Ok(Statement::AlterTable(AlterTableStmt {
        table,
        qualifier,
        if_exists,
        action,
    }))
}

fn alter_reloption_nodes(cmd: &pg_query::protobuf::AlterTableCmd) -> Result<&[Node]> {
    match cmd
        .def
        .as_deref()
        .and_then(|definition| definition.node.as_ref())
    {
        Some(NodeEnum::List(list)) => Ok(&list.items),
        other => Err(SQLError::Internal(format!(
            "ALTER relation options expected a list, got {other:?}"
        ))),
    }
}

fn collect_reset_reloption_names(nodes: &[Node], kind: AlterViewKind) -> Result<Vec<String>> {
    let mut names = Vec::with_capacity(nodes.len());
    let mut seen = std::collections::BTreeSet::new();
    for node in nodes {
        let Some(NodeEnum::DefElem(option)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER relation RESET contains a malformed option".into(),
            ));
        };
        let name = option.defname.to_ascii_lowercase();
        let recognized = match kind {
            AlterViewKind::View => {
                matches!(
                    name.as_str(),
                    "security_barrier" | "security_invoker" | "check_option"
                )
            }
            AlterViewKind::MaterializedView => name == "fillfactor",
        };
        if !recognized {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("unrecognized parameter \"{name}\""),
            });
        }
        if !seen.insert(name.clone()) {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("parameter \"{name}\" specified more than once"),
            });
        }
        names.push(name);
    }
    Ok(names)
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
        action,
    })
}
