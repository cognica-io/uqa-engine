//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_trigger`, `pg_rewrite`, and their definition helpers.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{
    BinaryOp, CreateRule, CreateTrigger, Expr, RuleEvent, TriggerEvent, TriggerTiming,
};
use uqa_sql::{ResultRow, SQLError};

use crate::engine_events::{StoredRule, StoredTrigger};
use crate::{Engine, RelationIdentity};

use super::helpers::{
    bool_value, catalog_usize, int_value, relation_oid, row, schema_expr_text, split_schema_name,
    stable_oid, str_value,
};
use super::pg_catalog::table_relation_oid;
use super::pg_proc::user_routine_catalog_oid;

const TRIGGER_TYPE_ROW: i64 = 1;
const TRIGGER_TYPE_BEFORE: i64 = 2;
const TRIGGER_TYPE_INSERT: i64 = 4;
const TRIGGER_TYPE_DELETE: i64 = 8;
const TRIGGER_TYPE_UPDATE: i64 = 16;
const TRIGGER_TYPE_TRUNCATE: i64 = 32;

pub(super) fn trigger_catalog_oid(trigger: &StoredTrigger) -> i64 {
    stable_oid(
        "trigger",
        &format!("{}.{}", trigger.definition.table, trigger.definition.name),
    )
}

pub(super) fn rule_catalog_oid(rule: &StoredRule) -> i64 {
    stable_oid(
        "rule",
        &format!("{}.{}", rule.definition.table, rule.definition.name),
    )
}

pub(super) fn build_pg_trigger(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    catalog_triggers(engine)?
        .into_iter()
        .map(|(trigger, parent_oid)| pg_trigger_row(engine, trigger, parent_oid))
        .collect()
}

fn catalog_triggers(engine: &Engine) -> Result<Vec<(StoredTrigger, i64)>, SQLError> {
    let originals = engine.list_triggers();
    let mut catalog = originals
        .iter()
        .cloned()
        .map(|trigger| {
            (
                (
                    trigger.definition.table.clone(),
                    trigger.definition.name.clone(),
                ),
                (trigger, 0),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for table in engine
        .table_names()
        .map_err(|error| SQLError::Internal(format!("read trigger tables: {error}")))?
    {
        let sources = engine.partition_trigger_sources(&table)?;
        let Some(parent) = sources.get(1) else {
            continue;
        };
        for source in sources.iter().skip(1) {
            for original in originals.iter().filter(|trigger| {
                trigger.definition.row && trigger.definition.table == source.qualified_name()
            }) {
                let mut clone = original.clone();
                clone.definition.table.clone_from(&table);
                let mut parent_clone = original.clone();
                parent_clone.definition.table = parent.qualified_name();
                catalog
                    .entry((table.clone(), clone.definition.name.clone()))
                    .or_insert((clone, trigger_catalog_oid(&parent_clone)));
            }
        }
    }
    Ok(catalog.into_values().collect())
}

fn pg_trigger_row(
    engine: &Engine,
    trigger: StoredTrigger,
    parent_oid: i64,
) -> Result<ResultRow, SQLError> {
    let definition = &trigger.definition;
    let function = engine.resolve_trigger_function(&definition.function)?;
    let columns = engine
        .try_describe_table(&definition.table)
        .map_err(|error| SQLError::Internal(format!("read trigger columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(definition.table.clone()))?;
    let attributes = definition
        .update_columns
        .iter()
        .map(|name| {
            columns
                .iter()
                .position(|column| column.name == *name)
                .ok_or_else(|| SQLError::UnknownColumn(name.clone()))
                .and_then(|index| {
                    i64::try_from(index + 1).map_err(|_| {
                        SQLError::Internal("trigger column position exceeds i64".into())
                    })
                })
                .map(Value::Int)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut arguments = Vec::new();
    for argument in &definition.arguments {
        arguments.extend_from_slice(argument.as_bytes());
        arguments.push(0);
    }
    Ok(row([
        ("oid", int_value(trigger_catalog_oid(&trigger))),
        (
            "tgrelid",
            int_value(table_relation_oid(engine, &definition.table)?),
        ),
        ("tgparentid", int_value(parent_oid)),
        ("tgname", str_value(definition.name.clone())),
        ("tgfoid", int_value(user_routine_catalog_oid(&function))),
        ("tgtype", int_value(trigger_type(definition))),
        ("tgenabled", str_value(trigger.enabled.catalog_code())),
        ("tgisinternal", bool_value(false)),
        ("tgconstrrelid", int_value(0)),
        ("tgconstrindid", int_value(0)),
        ("tgconstraint", int_value(0)),
        ("tgdeferrable", bool_value(false)),
        ("tginitdeferred", bool_value(false)),
        (
            "tgnargs",
            int_value(catalog_usize(
                definition.arguments.len(),
                "pg_trigger argument count",
            )?),
        ),
        ("tgattr", Value::List(attributes)),
        ("tgargs", Value::Bytes(arguments)),
        (
            "tgqual",
            definition.when.as_ref().map_or(Value::Null, |condition| {
                str_value(schema_expr_text(condition))
            }),
        ),
        ("tgoldtable", Value::Null),
        ("tgnewtable", Value::Null),
    ]))
}

fn rule_relation_oid(engine: &Engine, relation: &str) -> Result<i64, SQLError> {
    if engine
        .try_resolve_table_name(relation)
        .map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{relation}`: {error}"))
        })?
        .is_some()
    {
        return table_relation_oid(engine, relation);
    }
    let canonical = engine
        .try_resolve_view_name(relation)
        .map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{relation}`: {error}"))
        })?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let (schema, name) = split_schema_name(&canonical)?;
    Ok(relation_oid("v", &schema, &name))
}

pub(super) fn build_pg_rewrite(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = engine
        .list_rules()
        .into_iter()
        .map(|rule| {
            let definition = &rule.definition;
            Ok(row([
                ("oid", int_value(rule_catalog_oid(&rule))),
                ("rulename", str_value(definition.name.clone())),
                (
                    "ev_class",
                    int_value(rule_relation_oid(engine, &definition.table)?),
                ),
                ("ev_type", str_value(rule_event_code(definition.event))),
                ("ev_enabled", str_value(rule.enabled.catalog_code())),
                ("is_instead", bool_value(definition.instead)),
                (
                    "ev_qual",
                    definition.condition.as_ref().map_or_else(
                        || str_value("<>"),
                        |condition| str_value(schema_expr_text(condition)),
                    ),
                ),
                (
                    "ev_action",
                    str_value(serde_json::to_string(&definition.actions).map_err(|error| {
                        SQLError::Internal(format!("serialize rule action catalog: {error}"))
                    })?),
                ),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for name in engine.list_views()? {
        rows.push(row([
            (
                "oid",
                int_value(stable_oid("rule", &format!("{name}._RETURN"))),
            ),
            ("rulename", str_value("_RETURN")),
            ("ev_class", int_value(rule_relation_oid(engine, &name)?)),
            ("ev_type", str_value("1")),
            ("ev_enabled", str_value("O")),
            ("is_instead", bool_value(true)),
            ("ev_qual", str_value("<>")),
            (
                "ev_action",
                str_value(
                    engine
                        .view(&name)?
                        .map_or_else(|| "<>".to_string(), |query| format!("{query:?}")),
                ),
            ),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_rules(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    engine
        .list_rules()
        .into_iter()
        .filter(|rule| rule.definition.name != "_RETURN")
        .map(|rule| {
            let definition = &rule.definition;
            let (schema, table) = split_schema_name(&definition.table)?;
            Ok(row([
                ("schemaname", str_value(schema)),
                ("tablename", str_value(table)),
                ("rulename", str_value(definition.name.clone())),
                (
                    "definition",
                    str_value(render_rule_definition(engine, definition, false)?),
                ),
            ]))
        })
        .collect()
}

pub(in crate::sql) fn pg_get_triggerdef_value(
    engine: &Engine,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let definition_arguments = definition_arguments("pg_get_triggerdef", arguments)?;
    let Some((oid, pretty)) = definition_arguments else {
        return Ok(Value::Null);
    };
    let Some(trigger) = catalog_triggers(engine)?
        .into_iter()
        .map(|(trigger, _)| trigger)
        .find(|trigger| trigger_catalog_oid(trigger) == oid)
    else {
        return Ok(Value::Null);
    };
    Ok(str_value(render_trigger_definition(
        engine,
        &trigger.definition,
        pretty,
    )?))
}

pub(in crate::sql) fn pg_get_ruledef_value(
    engine: &Engine,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let definition_arguments = definition_arguments("pg_get_ruledef", arguments)?;
    let Some((oid, pretty)) = definition_arguments else {
        return Ok(Value::Null);
    };
    if let Some(rule) = engine
        .list_rules()
        .into_iter()
        .find(|rule| rule_catalog_oid(rule) == oid)
    {
        return Ok(str_value(render_rule_definition(
            engine,
            &rule.definition,
            pretty,
        )?));
    }
    for name in engine.list_views()? {
        if stable_oid("rule", &format!("{name}._RETURN")) == oid {
            return Ok(str_value(format!(
                "CREATE RULE \"_RETURN\" AS ON SELECT TO {} DO INSTEAD SELECT ...",
                render_rule_relation(engine, &name, pretty)?
            )));
        }
    }
    Ok(Value::Null)
}

fn definition_arguments(
    function: &str,
    arguments: &[Value],
) -> Result<Option<(i64, bool)>, SQLError> {
    if !(1..=2).contains(&arguments.len()) {
        return Err(SQLError::BadArity {
            name: function.into(),
            expected: "1 or 2".into(),
            actual: arguments.len(),
        });
    }
    let pretty = match arguments.get(1) {
        None => false,
        Some(Value::Bool(pretty)) => *pretty,
        Some(Value::Null) => return Ok(None),
        Some(pretty) => {
            return Err(SQLError::TypeMismatch(format!(
                "{function} pretty_bool must be boolean, got {pretty:?}"
            )))
        }
    };
    match arguments.first() {
        Some(Value::Null) => Ok(None),
        Some(Value::Int(oid)) => Ok(Some((*oid, pretty))),
        Some(value) => Err(SQLError::TypeMismatch(format!(
            "{function} object oid must be oid, got {value:?}"
        ))),
        None => unreachable!("arity was validated above"),
    }
}

fn trigger_type(definition: &CreateTrigger) -> i64 {
    let mut value = if definition.row { TRIGGER_TYPE_ROW } else { 0 };
    if definition.timing == TriggerTiming::Before {
        value |= TRIGGER_TYPE_BEFORE;
    }
    for event in &definition.events {
        value |= match event {
            TriggerEvent::Insert => TRIGGER_TYPE_INSERT,
            TriggerEvent::Delete => TRIGGER_TYPE_DELETE,
            TriggerEvent::Update => TRIGGER_TYPE_UPDATE,
            TriggerEvent::Truncate => TRIGGER_TYPE_TRUNCATE,
        };
    }
    value
}

const fn rule_event_code(event: RuleEvent) -> &'static str {
    match event {
        RuleEvent::Select => "1",
        RuleEvent::Update => "2",
        RuleEvent::Insert => "3",
        RuleEvent::Delete => "4",
    }
}

fn render_rule_definition(
    engine: &Engine,
    definition: &CreateRule,
    pretty: bool,
) -> Result<String, SQLError> {
    let mut rendered = format!(
        "CREATE RULE {} AS ON {} TO {}",
        uqa_sql::expr::quote_ident(&definition.name),
        match definition.event {
            RuleEvent::Select => "SELECT",
            RuleEvent::Insert => "INSERT",
            RuleEvent::Update => "UPDATE",
            RuleEvent::Delete => "DELETE",
        },
        render_rule_relation(engine, &definition.table, pretty)?
    );
    if let Some(condition) = &definition.condition {
        rendered.push_str(" WHERE (");
        rendered.push_str(&render_trigger_condition(condition, pretty));
        rendered.push(')');
    }
    rendered.push_str(if definition.instead {
        " DO INSTEAD"
    } else {
        " DO ALSO"
    });
    match definition.action_sql.as_slice() {
        [] => rendered.push_str(" NOTHING"),
        [action] => {
            rendered.push(' ');
            rendered.push_str(action);
        }
        actions => {
            rendered.push_str(" (");
            rendered.push_str(&actions.join("; "));
            rendered.push_str(";)");
        }
    }
    Ok(rendered)
}

fn render_rule_relation(engine: &Engine, name: &str, pretty: bool) -> Result<String, SQLError> {
    let relation = RelationIdentity::from_legacy_name(name)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{name}`: {error}")))?;
    if pretty {
        let local = uqa_sql::expr::quote_ident(&relation.name);
        let visible_table = engine.try_resolve_table_name(&local).map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{name}`: {error}"))
        })?;
        let visible_view = engine.try_resolve_view_name(&local).map_err(|error| {
            SQLError::Internal(format!("resolve rule relation `{name}`: {error}"))
        })?;
        if visible_table.as_deref() == Some(name) || visible_view.as_deref() == Some(name) {
            return Ok(local);
        }
    }
    Ok(render_qualified_name(name))
}

fn render_trigger_definition(
    engine: &Engine,
    definition: &CreateTrigger,
    pretty: bool,
) -> Result<String, SQLError> {
    let events = definition
        .events
        .iter()
        .map(|event| match event {
            TriggerEvent::Insert => "INSERT".to_string(),
            TriggerEvent::Delete => "DELETE".to_string(),
            TriggerEvent::Truncate => "TRUNCATE".to_string(),
            TriggerEvent::Update if definition.update_columns.is_empty() => "UPDATE".to_string(),
            TriggerEvent::Update => format!(
                "UPDATE OF {}",
                definition
                    .update_columns
                    .iter()
                    .map(|column| uqa_sql::expr::quote_ident(column))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let arguments = definition
        .arguments
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rendered = format!(
        "CREATE TRIGGER {} {} {} ON {} FOR EACH {}",
        uqa_sql::expr::quote_ident(&definition.name),
        match definition.timing {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
        },
        events,
        render_trigger_relation(engine, &definition.table, pretty)?,
        if definition.row { "ROW" } else { "STATEMENT" }
    );
    if let Some(condition) = &definition.when {
        rendered.push_str(" WHEN (");
        rendered.push_str(&render_trigger_condition(condition, pretty));
        rendered.push(')');
    }
    rendered.push_str(" EXECUTE FUNCTION ");
    rendered.push_str(&render_trigger_function(engine, &definition.function));
    rendered.push('(');
    rendered.push_str(&arguments);
    rendered.push(')');
    Ok(rendered)
}

fn render_trigger_relation(engine: &Engine, name: &str, pretty: bool) -> Result<String, SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("decode trigger relation `{name}`: {error}"))
    })?;
    if pretty {
        let local = uqa_sql::expr::quote_ident(&relation.name);
        let visible = engine.try_resolve_table_name(&local).map_err(|error| {
            SQLError::Internal(format!("resolve trigger relation `{name}`: {error}"))
        })?;
        if visible.as_deref() == Some(name) {
            return Ok(local);
        }
    }
    Ok(render_qualified_name(name))
}

fn render_trigger_function(engine: &Engine, name: &str) -> String {
    let Ok(function) = RelationIdentity::from_legacy_name(name) else {
        return render_qualified_name(name);
    };
    let local = uqa_sql::expr::quote_ident(&function.name);
    if engine
        .resolve_trigger_function(&local)
        .is_ok_and(|visible| visible.def.name == name)
    {
        local
    } else {
        render_qualified_name(name)
    }
}

fn render_trigger_condition(condition: &Expr, pretty: bool) -> String {
    if pretty {
        render_pretty_expr(condition, 0)
    } else {
        schema_expr_text(condition)
    }
}

fn render_pretty_expr(expr: &Expr, parent_precedence: u8) -> String {
    let (precedence, rendered) = match expr {
        Expr::Or(items) => (
            1,
            items
                .iter()
                .map(|item| render_pretty_expr(item, 1))
                .collect::<Vec<_>>()
                .join(" OR "),
        ),
        Expr::And(items) => (
            2,
            items
                .iter()
                .map(|item| render_pretty_expr(item, 2))
                .collect::<Vec<_>>()
                .join(" AND "),
        ),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Func { name, args, .. } if name == "__is_distinct" && args.len() == 2 => (
                4,
                format!(
                    "{} IS NOT DISTINCT FROM {}",
                    render_pretty_expr(&args[0], 5),
                    render_pretty_expr(&args[1], 5)
                ),
            ),
            _ => (3, format!("NOT {}", render_pretty_expr(inner, 3))),
        },
        Expr::Binary { op, lhs, rhs } => {
            let (precedence, operator) = match op {
                BinaryOp::Equal => (4, "="),
                BinaryOp::NotEqual => (4, "<>"),
                BinaryOp::Less => (4, "<"),
                BinaryOp::LessEqual => (4, "<="),
                BinaryOp::Greater => (4, ">"),
                BinaryOp::GreaterEqual => (4, ">="),
                BinaryOp::Add => (5, "+"),
                BinaryOp::Subtract => (5, "-"),
                BinaryOp::Multiply => (6, "*"),
                BinaryOp::Divide => (6, "/"),
            };
            let rhs_precedence = if matches!(op, BinaryOp::Subtract | BinaryOp::Divide) {
                precedence + 1
            } else {
                precedence
            };
            (
                precedence,
                format!(
                    "{} {operator} {}",
                    render_pretty_expr(lhs, precedence),
                    render_pretty_expr(rhs, rhs_precedence)
                ),
            )
        }
        Expr::Func { name, args, .. } if name == "__is_distinct" && args.len() == 2 => (
            4,
            format!(
                "{} IS DISTINCT FROM {}",
                render_pretty_expr(&args[0], 5),
                render_pretty_expr(&args[1], 5)
            ),
        ),
        Expr::IsNull { expr, negated } => (
            4,
            format!(
                "{} IS {}NULL",
                render_pretty_expr(expr, 5),
                if *negated { "NOT " } else { "" }
            ),
        ),
        Expr::Between { expr, low, high } => (
            4,
            format!(
                "{} BETWEEN {} AND {}",
                render_pretty_expr(expr, 5),
                render_pretty_expr(low, 5),
                render_pretty_expr(high, 5)
            ),
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => (
            4,
            format!(
                "{} {}IN ({})",
                render_pretty_expr(expr, 5),
                if *negated { "NOT " } else { "" },
                list.iter()
                    .map(|item| render_pretty_expr(item, 0))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Expr::UnaryMinus(inner) => (7, format!("-{}", render_pretty_expr(inner, 7))),
        _ => (8, schema_expr_text(expr)),
    };
    if precedence < parent_precedence {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn render_qualified_name(name: &str) -> String {
    name.split('.')
        .map(uqa_sql::expr::quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}
