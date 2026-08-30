//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE/DROP TRIGGER` and `CREATE/DROP RULE` lowering.

use pg_query::protobuf::{CmdType, DropBehavior};
use pg_query::TriggerType;

use crate::ast::{
    CreateRule, CreateTrigger, DropRule, DropTrigger, RuleEvent, TriggerDeferrability,
    TriggerEvent, TriggerTiming, TriggerTransitionRelation,
};

use super::dispatch::compile_stmt;

use super::{
    compile_expr, compile_qualified_name, extract_string, range_var_name, NodeEnum, Result,
    SQLError,
};

pub(super) fn compile_create_trigger(
    stmt: &pg_query::protobuf::CreateTrigStmt,
) -> Result<CreateTrigger> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TRIGGER without relation".into()))?;
    let transition_relations = stmt
        .transition_rels
        .iter()
        .map(|node| {
            let Some(NodeEnum::TriggerTransition(relation)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "CREATE TRIGGER REFERENCING entry is not a transition relation".into(),
                ));
            };
            Ok(TriggerTransitionRelation {
                name: relation.name.clone(),
                is_new: relation.is_new,
                is_table: relation.is_table,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let timing = match stmt.timing {
        value if value == TriggerType::Before as i32 => TriggerTiming::Before,
        0 => TriggerTiming::After,
        value if value == TriggerType::Instead as i32 => TriggerTiming::InsteadOf,
        value => {
            return Err(SQLError::Internal(format!(
                "CREATE TRIGGER has unknown timing bit {value}"
            )))
        }
    };
    let mut events = Vec::new();
    for (bit, event) in [
        (TriggerType::Insert as i32, TriggerEvent::Insert),
        (TriggerType::Update as i32, TriggerEvent::Update),
        (TriggerType::Delete as i32, TriggerEvent::Delete),
        (TriggerType::Truncate as i32, TriggerEvent::Truncate),
    ] {
        if stmt.events & bit != 0 {
            events.push(event);
        }
    }
    if events.is_empty() {
        return Err(SQLError::Internal("CREATE TRIGGER without an event".into()));
    }
    let arguments = stmt
        .args
        .iter()
        .map(extract_string)
        .collect::<Result<_>>()?;
    let update_columns = stmt
        .columns
        .iter()
        .map(extract_string)
        .collect::<Result<_>>()?;
    let when = stmt.when_clause.as_deref().map(compile_expr).transpose()?;
    Ok(CreateTrigger {
        name: stmt.trigname.clone(),
        table: range_var_name(relation),
        function: compile_qualified_name(&stmt.funcname, "CREATE TRIGGER")?,
        arguments,
        constraint: stmt.isconstraint,
        referenced_table: stmt.constrrel.as_ref().map(range_var_name),
        deferrability: match (stmt.deferrable, stmt.initdeferred) {
            (false, false) => TriggerDeferrability::NotDeferrable,
            (true, false) => TriggerDeferrability::InitiallyImmediate,
            (true, true) => TriggerDeferrability::InitiallyDeferred,
            (false, true) => {
                return Err(SQLError::Internal(
                    "CREATE CONSTRAINT TRIGGER retained INITIALLY DEFERRED without DEFERRABLE"
                        .into(),
                ));
            }
        },
        row: stmt.row,
        timing,
        events,
        update_columns,
        transition_relations,
        when,
        or_replace: stmt.replace,
    })
}

pub(super) fn compile_create_rule(stmt: &pg_query::protobuf::RuleStmt) -> Result<CreateRule> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE RULE without relation".into()))?;
    let event = match stmt.event() {
        CmdType::CmdSelect => RuleEvent::Select,
        CmdType::CmdInsert => RuleEvent::Insert,
        CmdType::CmdUpdate => RuleEvent::Update,
        CmdType::CmdDelete => RuleEvent::Delete,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE RULE event {other:?} is not implemented"
            )))
        }
    };
    let condition = stmt.where_clause.as_deref().map(compile_expr).transpose()?;
    let actions = stmt
        .actions
        .iter()
        .map(compile_stmt)
        .collect::<Result<Vec<_>>>()?;
    let action_sql = stmt
        .actions
        .iter()
        .map(|action| {
            action
                .deparse()
                .map_err(|error| SQLError::Internal(format!("deparse CREATE RULE action: {error}")))
        })
        .collect::<Result<Vec<_>>>()?;
    for action in &actions {
        if !matches!(
            action,
            crate::ast::Statement::Select(_)
                | crate::ast::Statement::Insert(_)
                | crate::ast::Statement::Update(_)
                | crate::ast::Statement::Delete(_)
        ) {
            return Err(SQLError::Unsupported(
                "rewrite-rule actions currently support SELECT, INSERT, UPDATE, and DELETE".into(),
            ));
        }
    }
    Ok(CreateRule {
        name: stmt.rulename.clone(),
        table: range_var_name(relation),
        event,
        instead: stmt.instead,
        condition,
        actions,
        action_sql,
        or_replace: stmt.replace,
    })
}

fn compile_drop_relation_component(
    stmt: &pg_query::protobuf::DropStmt,
    statement: &str,
) -> Result<(String, String)> {
    if stmt.objects.len() != 1 {
        return Err(SQLError::Unsupported(format!(
            "{statement} accepts exactly one object"
        )));
    }
    let object = stmt
        .objects
        .first()
        .ok_or_else(|| SQLError::Internal(format!("{statement} without an object")))?;
    let NodeEnum::List(parts) = object
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{statement} has an empty object")))?
    else {
        return Err(SQLError::Internal(format!(
            "{statement} object is not a qualified name"
        )));
    };
    let mut parts = parts
        .items
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    let name = parts
        .pop()
        .ok_or_else(|| SQLError::Internal(format!("{statement} without an object name")))?;
    if !(1..=2).contains(&parts.len()) {
        return Err(SQLError::Unsupported(format!(
            "{statement}: cross-database relation names are not supported"
        )));
    }
    let table = parts
        .iter()
        .map(|part| super::render_relation_component(part))
        .collect::<Vec<_>>()
        .join(".");
    Ok((name, table))
}

pub(super) fn compile_drop_trigger(stmt: &pg_query::protobuf::DropStmt) -> Result<DropTrigger> {
    let (name, table) = compile_drop_relation_component(stmt, "DROP TRIGGER")?;
    Ok(DropTrigger {
        name,
        table,
        if_exists: stmt.missing_ok,
        cascade: matches!(stmt.behavior(), DropBehavior::DropCascade),
    })
}

pub(super) fn compile_drop_rule(stmt: &pg_query::protobuf::DropStmt) -> Result<DropRule> {
    let (name, table) = compile_drop_relation_component(stmt, "DROP RULE")?;
    Ok(DropRule {
        name,
        table,
        if_exists: stmt.missing_ok,
        cascade: matches!(stmt.behavior(), DropBehavior::DropCascade),
    })
}
