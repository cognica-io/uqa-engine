//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP, ALTER TABLE, and RENAME lowering.

use super::relations::{
    collect_def_elem_options, validate_materialized_view_options, validate_view_options,
};
use super::routines::{compile_drop_function, compile_object_with_args, CompiledRoutineTarget};
use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, validate_foreign_key_set_columns,
};
use super::{
    compile_column_def, compile_expr, compile_pg_type_name, extract_string, range_var_name,
    render_relation_component, AlterTableAction, AlterTableStmt, AlterViewAction, AlterViewKind,
    AlterViewStmt, DropKind, DropStmt, Node, NodeEnum, Result, SQLError, Statement,
    TableKeyConstraint, TableKeyConstraintKind,
};
use crate::ast::{
    AlterRoutineKind, AlterSequence, EventEnableMode, ForeignKey, RelationPersistence,
    RenameRoutineStmt, SequenceLifecycle, TableCheck,
};

fn extract_strings(nodes: &[pg_query::protobuf::Node]) -> Result<Vec<String>> {
    nodes.iter().map(extract_string).collect()
}

fn trigger_enable_action(command: &pg_query::protobuf::AlterTableCmd) -> Option<AlterTableAction> {
    use pg_query::protobuf::AlterTableType;
    let subtype = command.subtype();
    match subtype {
        AlterTableType::AtEnableTrig
        | AlterTableType::AtEnableAlwaysTrig
        | AlterTableType::AtEnableReplicaTrig
        | AlterTableType::AtDisableTrig => Some(AlterTableAction::SetTriggerEnableMode {
            name: Some(command.name.clone()),
            user_only: false,
            mode: match subtype {
                AlterTableType::AtEnableTrig => EventEnableMode::Origin,
                AlterTableType::AtEnableAlwaysTrig => EventEnableMode::Always,
                AlterTableType::AtEnableReplicaTrig => EventEnableMode::Replica,
                AlterTableType::AtDisableTrig => EventEnableMode::Disabled,
                _ => unreachable!("named trigger modes were matched above"),
            },
        }),
        AlterTableType::AtEnableTrigAll
        | AlterTableType::AtDisableTrigAll
        | AlterTableType::AtEnableTrigUser
        | AlterTableType::AtDisableTrigUser => Some(AlterTableAction::SetTriggerEnableMode {
            name: None,
            user_only: matches!(
                subtype,
                AlterTableType::AtEnableTrigUser | AlterTableType::AtDisableTrigUser
            ),
            mode: if matches!(
                subtype,
                AlterTableType::AtEnableTrigAll | AlterTableType::AtEnableTrigUser
            ) {
                EventEnableMode::Origin
            } else {
                EventEnableMode::Disabled
            },
        }),
        _ => None,
    }
}

pub(super) fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    if stmt.remove_type() == ObjectType::ObjectTrigger {
        return super::events::compile_drop_trigger(stmt).map(Statement::DropTrigger);
    }
    if stmt.remove_type() == ObjectType::ObjectRule {
        return super::events::compile_drop_rule(stmt).map(Statement::DropRule);
    }
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectForeignTable => DropKind::ForeignTable,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectMatview => DropKind::MaterializedView,
        ObjectType::ObjectSequence => DropKind::Sequence,
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
                    DropKind::Table
                        | DropKind::ForeignTable
                        | DropKind::Index
                        | DropKind::View
                        | DropKind::MaterializedView
                        | DropKind::Sequence
                ) {
                    if parts.len() > 2 {
                        return Err(SQLError::Routine {
                            sqlstate: "42601".into(),
                            message: format!(
                                "improper qualified name (too many dotted names): {}",
                                parts.join(".")
                            ),
                        });
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

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(super) fn compile_alter_table(stmt: &pg_query::protobuf::AlterTableStmt) -> Result<Statement> {
    use pg_query::protobuf::{AlterTableType, DropBehavior, ObjectType};
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
    if stmt.objtype() == ObjectType::ObjectSequence {
        let [command] = stmt.cmds.as_slice() else {
            return Err(SQLError::Unsupported(
                "ALTER SEQUENCE SET persistence accepts one action".into(),
            ));
        };
        let Some(NodeEnum::AlterTableCmd(command)) = command.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE SET persistence command body is malformed".into(),
            ));
        };
        let mut alter = AlterSequence {
            name: table,
            if_exists,
            ..AlterSequence::default()
        };
        match command.subtype() {
            AlterTableType::AtSetLogged => {
                alter.persistence = Some(RelationPersistence::Permanent);
            }
            AlterTableType::AtSetUnLogged => {
                alter.persistence = Some(RelationPersistence::Unlogged);
            }
            AlterTableType::AtChangeOwner => {
                let owner = command.newowner.as_ref().ok_or_else(|| {
                    SQLError::Internal("ALTER SEQUENCE OWNER TO without owner".into())
                })?;
                alter.role_owner = Some(super::routines::compile_role_spec(
                    owner,
                    false,
                    "ALTER SEQUENCE OWNER TO",
                )?);
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER SEQUENCE action {other:?} is not supported"
                )))
            }
        }
        return Ok(Statement::AlterSequence(alter));
    }
    if stmt.objtype() == ObjectType::ObjectForeignTable {
        let mut trigger_actions = Vec::with_capacity(stmt.cmds.len());
        for command in &stmt.cmds {
            let Some(NodeEnum::AlterTableCmd(command)) = command.node.as_ref() else {
                return Err(SQLError::Internal(
                    "ALTER FOREIGN TABLE command body is malformed".into(),
                ));
            };
            let Some(action) = trigger_enable_action(command) else {
                trigger_actions.clear();
                break;
            };
            trigger_actions.push(action);
        }
        if trigger_actions.len() == stmt.cmds.len() {
            return Ok(Statement::AlterTable(AlterTableStmt {
                table,
                qualifier,
                if_exists,
                recurse: false,
                actions: trigger_actions,
            }));
        }
        let [command] = stmt.cmds.as_slice() else {
            return Err(SQLError::Unsupported(
                "ALTER FOREIGN TABLE OWNER TO accepts one action".into(),
            ));
        };
        let Some(NodeEnum::AlterTableCmd(command)) = command.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER FOREIGN TABLE command body is malformed".into(),
            ));
        };
        if command.subtype() != AlterTableType::AtChangeOwner {
            return Err(SQLError::Unsupported(format!(
                "ALTER FOREIGN TABLE action {:?} is not supported",
                command.subtype()
            )));
        }
        let owner = command.newowner.as_ref().ok_or_else(|| {
            SQLError::Internal("ALTER FOREIGN TABLE OWNER TO without owner".into())
        })?;
        return Ok(Statement::AlterForeignTable(
            crate::ast::AlterForeignTableStmt {
                name: table,
                if_exists,
                owner: super::routines::compile_role_spec(
                    owner,
                    false,
                    "ALTER FOREIGN TABLE OWNER TO",
                )?,
            },
        ));
    }
    if matches!(
        stmt.objtype(),
        ObjectType::ObjectView | ObjectType::ObjectMatview
    ) {
        let [command] = stmt.cmds.as_slice() else {
            return Err(SQLError::Unsupported(
                "ALTER VIEW accepts one action at a time".into(),
            ));
        };
        let inner = command
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ALTER VIEW command body empty".into()))?;
        let NodeEnum::AlterTableCmd(cmd) = inner else {
            return Err(SQLError::Unsupported(format!(
                "ALTER VIEW command {inner:?}"
            )));
        };
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
                AlterViewAction::Set(options)
            }
            AlterTableType::AtResetRelOptions => {
                let nodes = alter_reloption_nodes(cmd)?;
                AlterViewAction::Reset(collect_reset_reloption_names(nodes, kind)?)
            }
            AlterTableType::AtChangeOwner => {
                let owner = cmd.newowner.as_ref().ok_or_else(|| {
                    SQLError::Internal("ALTER VIEW OWNER TO without owner".into())
                })?;
                AlterViewAction::OwnerTo(super::routines::compile_role_spec(
                    owner,
                    false,
                    "ALTER VIEW OWNER TO",
                )?)
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
        return Ok(Statement::AlterView(AlterViewStmt {
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
            AlterTableType::AtSetLogged => AlterTableAction::SetPersistence {
                persistence: RelationPersistence::Permanent,
            },
            AlterTableType::AtSetUnLogged => AlterTableAction::SetPersistence {
                persistence: RelationPersistence::Unlogged,
            },
            AlterTableType::AtChangeOwner => {
                let owner = cmd.newowner.as_ref().ok_or_else(|| {
                    SQLError::Internal("ALTER TABLE OWNER TO without owner".into())
                })?;
                AlterTableAction::ChangeOwner {
                    owner: super::routines::compile_role_spec(
                        owner,
                        false,
                        "ALTER TABLE OWNER TO",
                    )?,
                }
            }
            AlterTableType::AtEnableTrig
            | AlterTableType::AtEnableAlwaysTrig
            | AlterTableType::AtEnableReplicaTrig
            | AlterTableType::AtDisableTrig => AlterTableAction::SetTriggerEnableMode {
                name: Some(cmd.name.clone()),
                user_only: false,
                mode: match cmd.subtype() {
                    AlterTableType::AtEnableTrig => EventEnableMode::Origin,
                    AlterTableType::AtEnableAlwaysTrig => EventEnableMode::Always,
                    AlterTableType::AtEnableReplicaTrig => EventEnableMode::Replica,
                    AlterTableType::AtDisableTrig => EventEnableMode::Disabled,
                    _ => unreachable!("trigger enable modes were matched above"),
                },
            },
            AlterTableType::AtEnableRule
            | AlterTableType::AtEnableAlwaysRule
            | AlterTableType::AtEnableReplicaRule
            | AlterTableType::AtDisableRule => AlterTableAction::SetRuleEnableMode {
                name: cmd.name.clone(),
                mode: match cmd.subtype() {
                    AlterTableType::AtEnableRule => EventEnableMode::Origin,
                    AlterTableType::AtEnableAlwaysRule => EventEnableMode::Always,
                    AlterTableType::AtEnableReplicaRule => EventEnableMode::Replica,
                    AlterTableType::AtDisableRule => EventEnableMode::Disabled,
                    _ => unreachable!("rule enable modes were matched above"),
                },
            },
            AlterTableType::AtEnableTrigAll
            | AlterTableType::AtDisableTrigAll
            | AlterTableType::AtEnableTrigUser
            | AlterTableType::AtDisableTrigUser => AlterTableAction::SetTriggerEnableMode {
                name: None,
                user_only: matches!(
                    cmd.subtype(),
                    AlterTableType::AtEnableTrigUser | AlterTableType::AtDisableTrigUser
                ),
                mode: if matches!(
                    cmd.subtype(),
                    AlterTableType::AtEnableTrigAll | AlterTableType::AtEnableTrigUser
                ) {
                    EventEnableMode::Origin
                } else {
                    EventEnableMode::Disabled
                },
            },
            AlterTableType::AtAddInherit | AlterTableType::AtDropInherit => {
                let parent = match cmd
                    .def
                    .as_deref()
                    .and_then(|definition| definition.node.as_ref())
                {
                    Some(NodeEnum::RangeVar(parent)) => range_var_name(parent),
                    other => {
                        return Err(SQLError::Internal(format!(
                            "ALTER TABLE inheritance parent has unexpected node {other:?}"
                        )))
                    }
                };
                if cmd.subtype() == AlterTableType::AtAddInherit {
                    AlterTableAction::AddInheritance { parent }
                } else {
                    AlterTableAction::DropInheritance { parent }
                }
            }
            AlterTableType::AtAttachPartition => {
                let partition = match cmd
                    .def
                    .as_deref()
                    .and_then(|definition| definition.node.as_ref())
                {
                    Some(NodeEnum::PartitionCmd(partition)) => partition,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "ATTACH PARTITION has unexpected command node {other:?}"
                        )))
                    }
                };
                let name = partition.name.as_ref().ok_or_else(|| {
                    SQLError::Internal("ATTACH PARTITION without relation".into())
                })?;
                let bound = partition.bound.as_ref().ok_or_else(|| {
                    SQLError::Internal("ATTACH PARTITION without FOR VALUES".into())
                })?;
                AlterTableAction::AttachPartition {
                    partition: range_var_name(name),
                    bound: super::hierarchy::compile_partition_bound(bound)?,
                }
            }
            AlterTableType::AtDetachPartition | AlterTableType::AtDetachPartitionFinalize => {
                let partition = match cmd
                    .def
                    .as_deref()
                    .and_then(|definition| definition.node.as_ref())
                {
                    Some(NodeEnum::PartitionCmd(partition)) => partition,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "DETACH PARTITION has unexpected command node {other:?}"
                        )))
                    }
                };
                let name = partition.name.as_ref().ok_or_else(|| {
                    SQLError::Internal("DETACH PARTITION without relation".into())
                })?;
                AlterTableAction::DetachPartition {
                    partition: range_var_name(name),
                    concurrently: partition.concurrent,
                    finalize: cmd.subtype() == AlterTableType::AtDetachPartitionFinalize,
                }
            }
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
                                partition_constraint: None,
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
                            object_id: None,
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
    Ok(Statement::AlterTable(AlterTableStmt {
        table,
        qualifier,
        if_exists,
        recurse: relation.inh,
        actions,
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

pub(super) fn compile_rename(stmt: &pg_query::protobuf::RenameStmt) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;
    let (routine_kind, context) = match stmt.rename_type() {
        ObjectType::ObjectFunction => (Some(AlterRoutineKind::Function), "ALTER FUNCTION"),
        ObjectType::ObjectProcedure => (Some(AlterRoutineKind::Procedure), "ALTER PROCEDURE"),
        ObjectType::ObjectRoutine => (Some(AlterRoutineKind::Routine), "ALTER ROUTINE"),
        _ => (None, "RENAME"),
    };
    if let Some(kind) = routine_kind {
        let Some(NodeEnum::ObjectWithArgs(object)) = stmt
            .object
            .as_deref()
            .and_then(|object| object.node.as_ref())
        else {
            return Err(SQLError::Internal(format!(
                "{context}: malformed routine target"
            )));
        };
        let CompiledRoutineTarget {
            name,
            arg_types,
            arg_type_references,
        } = compile_object_with_args(object, context)?;
        return Ok(Statement::RenameRoutine(RenameRoutineStmt {
            kind,
            name,
            arg_types,
            arg_type_references,
            new_name: render_relation_component(&stmt.newname),
        }));
    }
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
        ObjectType::ObjectSequence => {
            return Ok(Statement::AlterSequence(AlterSequence {
                name: table,
                if_exists: stmt.missing_ok,
                lifecycle: SequenceLifecycle::RenameTo {
                    name: render_relation_component(&stmt.newname),
                },
                ..AlterSequence::default()
            }));
        }
        ObjectType::ObjectTrigger => AlterTableAction::RenameTrigger {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectTabconstraint => AlterTableAction::RenameConstraint {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectRule => AlterTableAction::RenameRule {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        other => {
            return Err(SQLError::Unsupported(format!(
                "RENAME target {other:?} not supported"
            )));
        }
    };
    Ok(Statement::AlterTable(AlterTableStmt {
        table,
        qualifier: relation.relname.clone(),
        if_exists: stmt.missing_ok,
        recurse: false,
        actions: vec![action],
    }))
}

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
        other => Err(SQLError::Unsupported(format!(
            "ALTER {other:?} SET SCHEMA is not supported"
        ))),
    }
}
