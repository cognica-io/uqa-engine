//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_trigger` and `pg_get_triggerdef` catalog support.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{CreateTrigger, TriggerEvent, TriggerTiming};
use uqa_sql::{ResultRow, SQLError};

use crate::engine_events::StoredTrigger;
use crate::Engine;

use super::helpers::{
    bool_value, catalog_usize, int_value, row, schema_expr_text, stable_oid, str_value,
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

pub(in crate::sql) fn pg_get_triggerdef_value(
    engine: &Engine,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let oid = definition_oid_argument("pg_get_triggerdef", arguments)?;
    let Some(oid) = oid else {
        return Ok(Value::Null);
    };
    Ok(catalog_triggers(engine)?
        .into_iter()
        .map(|(trigger, _)| trigger)
        .find(|trigger| trigger_catalog_oid(trigger) == oid)
        .map_or(Value::Null, |trigger| {
            str_value(render_trigger_definition(&trigger.definition))
        }))
}

fn definition_oid_argument(function: &str, arguments: &[Value]) -> Result<Option<i64>, SQLError> {
    if !(1..=2).contains(&arguments.len()) {
        return Err(SQLError::BadArity {
            name: function.into(),
            expected: "1 or 2".into(),
            actual: arguments.len(),
        });
    }
    if let Some(pretty) = arguments.get(1) {
        if !matches!(pretty, Value::Bool(_) | Value::Null) {
            return Err(SQLError::TypeMismatch(format!(
                "{function} pretty_bool must be boolean, got {pretty:?}"
            )));
        }
    }
    match &arguments[0] {
        Value::Null => Ok(None),
        Value::Int(oid) => Ok(Some(*oid)),
        value => Err(SQLError::TypeMismatch(format!(
            "{function} trigger oid must be oid, got {value:?}"
        ))),
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

fn render_trigger_definition(definition: &CreateTrigger) -> String {
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
        render_qualified_name(&definition.table),
        if definition.row { "ROW" } else { "STATEMENT" }
    );
    if let Some(condition) = &definition.when {
        rendered.push_str(" WHEN (");
        rendered.push_str(&schema_expr_text(condition));
        rendered.push(')');
    }
    rendered.push_str(" EXECUTE FUNCTION ");
    rendered.push_str(&render_qualified_name(&definition.function));
    rendered.push('(');
    rendered.push_str(&arguments);
    rendered.push(')');
    rendered
}

fn render_qualified_name(name: &str) -> String {
    name.split('.')
        .map(uqa_sql::expr::quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}
