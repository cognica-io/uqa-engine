//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_trigger`, `pg_rewrite`, and their definition helpers.

mod rules;

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{
    BinaryOp, CreateTrigger, Expr, FunctionDispatch, RuleEvent, TriggerEvent, TriggerTiming,
};
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::engine_events::{StoredRule, StoredTrigger};
use crate::engine_user_functions::{
    canonical_routine_type_name, routine_signature_types, CompiledFunctionBody, SQLUserFunction,
};
use crate::{Engine, RelationIdentity};

use super::expression_text::schema_expr_text;
use super::helpers::oids::{relation_oid, schema_oid, split_schema_name, stable_oid};
use super::helpers::rows::{bool_value, catalog_usize, int_value, row, str_value};
use super::helpers::views::view_columns_for;
use super::pg_catalog::table_relation_oid_from;
use super::pg_proc::user_routine_catalog_oid;

use rules::{render_rule_definition, render_rule_relation, rule_condition_text};

const TRIGGER_TYPE_ROW: i64 = 1;
const TRIGGER_TYPE_BEFORE: i64 = 2;
const TRIGGER_TYPE_INSERT: i64 = 4;
const TRIGGER_TYPE_DELETE: i64 = 8;
const TRIGGER_TYPE_UPDATE: i64 = 16;
const TRIGGER_TYPE_TRUNCATE: i64 = 32;
const TRIGGER_TYPE_INSTEAD: i64 = 64;

pub(in crate::sql) fn event_relation_oid(engine: &Engine, relation: &str) -> Result<i64, SQLError> {
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    event_relation_oid_from(&catalog, &resolution, relation)
}

fn event_relation_oid_from(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &str,
) -> Result<i64, SQLError> {
    if catalog.table_name_resolved(resolution, relation)?.is_some() {
        return table_relation_oid_from(catalog, resolution, relation);
    }
    if let Some((canonical, _)) = catalog.foreign_table_entry_resolved(resolution, relation)? {
        let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!(
                "decode foreign trigger relation `{canonical}`: {error}"
            ))
        })?;
        return Ok(super::foreign_table_relation_oid(&relation));
    }
    let canonical = catalog
        .view_name_resolved(resolution, relation)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let (schema, name) = split_schema_name(&canonical)?;
    Ok(relation_oid("v", &schema, &name))
}

pub(super) fn trigger_catalog_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: &StoredTrigger,
) -> Result<i64, SQLError> {
    let identity = if let Some(object_id) = trigger.object_id {
        format!(
            "{}:{}",
            hex_object_id(object_id),
            event_relation_oid_from(catalog, resolution, &trigger.definition.table)?
        )
    } else {
        format!("{}.{}", trigger.definition.table, trigger.definition.name)
    };
    Ok(stable_oid("trigger", &identity))
}

pub(super) fn trigger_constraint_catalog_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: &StoredTrigger,
) -> Result<i64, SQLError> {
    if !trigger.definition.constraint {
        return Ok(0);
    }
    let constraint_name = trigger
        .constraint_name
        .as_deref()
        .unwrap_or(&trigger.definition.name);
    let identity = if let Some(object_id) = trigger.object_id {
        format!(
            "{}:{}",
            hex_object_id(object_id),
            event_relation_oid_from(catalog, resolution, &trigger.definition.table)?
        )
    } else {
        format!("{}.{}", trigger.definition.table, constraint_name)
    };
    Ok(stable_oid("constraint", &identity))
}

fn hex_object_id(object_id: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(32);
    for byte in object_id {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn rule_catalog_oid(rule: &StoredRule) -> i64 {
    stable_oid(
        "rule",
        &format!("{}.{}", rule.definition.table, rule.definition.name),
    )
}

pub(super) fn build_pg_trigger(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    catalog_triggers(catalog, resolution)?
        .into_iter()
        .map(|(trigger, parent_oid)| {
            pg_trigger_row(engine, catalog, resolution, trigger, parent_oid)
        })
        .collect()
}

pub(super) fn catalog_triggers(
    catalog_view: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<(StoredTrigger, i64)>, SQLError> {
    let originals = catalog_view.triggers();
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
    for table in catalog_view.table_names() {
        let sources = catalog_view.partition_trigger_sources(resolution, &table)?;
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
                    .or_insert((
                        clone,
                        trigger_catalog_oid(catalog_view, resolution, &parent_clone)?,
                    ));
            }
        }
    }
    Ok(catalog.into_values().collect())
}

pub(super) fn build_trigger_constraints(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for (trigger, _) in catalog_triggers(catalog, resolution)? {
        if !trigger.definition.constraint {
            continue;
        }
        let definition = &trigger.definition;
        let constraint_name = trigger
            .constraint_name
            .as_deref()
            .unwrap_or(&definition.name);
        let relation = RelationIdentity::from_legacy_name(&definition.table).map_err(|error| {
            SQLError::Internal(format!(
                "decode constraint-trigger relation `{}`: {error}",
                definition.table
            ))
        })?;
        rows.push(row([
            (
                "oid",
                int_value(trigger_constraint_catalog_oid(
                    catalog, resolution, &trigger,
                )?),
            ),
            ("conname", str_value(constraint_name)),
            ("connamespace", int_value(schema_oid(&relation.schema))),
            ("contype", str_value("t")),
            (
                "condeferrable",
                bool_value(definition.deferrability.is_deferrable()),
            ),
            (
                "condeferred",
                bool_value(definition.deferrability.is_initially_deferred()),
            ),
            ("conenforced", bool_value(true)),
            ("convalidated", bool_value(true)),
            (
                "conrelid",
                int_value(table_relation_oid_from(
                    catalog,
                    resolution,
                    &definition.table,
                )?),
            ),
            ("contypid", int_value(0)),
            ("conindid", int_value(0)),
            ("conparentid", int_value(0)),
            ("confrelid", int_value(0)),
            ("confupdtype", str_value(" ")),
            ("confdeltype", str_value(" ")),
            ("confmatchtype", str_value(" ")),
            ("conislocal", bool_value(true)),
            ("coninhcount", int_value(0)),
            ("connoinherit", bool_value(true)),
            ("conperiod", bool_value(false)),
            ("conkey", Value::Null),
            ("confkey", Value::Null),
            ("conpfeqop", Value::Null),
            ("conppeqop", Value::Null),
            ("conffeqop", Value::Null),
            ("conexclop", Value::Null),
            ("conbin", Value::Null),
        ]));
    }
    Ok(rows)
}

fn pg_trigger_row(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: StoredTrigger,
    parent_oid: i64,
) -> Result<ResultRow, SQLError> {
    let definition = &trigger.definition;
    let function = resolve_trigger_function(catalog, resolution, &definition.function)?;
    let columns = event_relation_columns(engine, catalog, resolution, &definition.table)?;
    let attributes = definition
        .update_columns
        .iter()
        .map(|name| {
            columns
                .iter()
                .position(|(column, _)| column == name)
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
    let constraint_oid = trigger_constraint_catalog_oid(catalog, resolution, &trigger)?;
    let referenced_relation_oid = definition
        .referenced_table
        .as_deref()
        .map(|table| table_relation_oid_from(catalog, resolution, table))
        .transpose()?
        .unwrap_or(0);
    Ok(row([
        (
            "oid",
            int_value(trigger_catalog_oid(catalog, resolution, &trigger)?),
        ),
        (
            "tgrelid",
            int_value(event_relation_oid_from(
                catalog,
                resolution,
                &definition.table,
            )?),
        ),
        ("tgparentid", int_value(parent_oid)),
        ("tgname", str_value(definition.name.clone())),
        ("tgfoid", int_value(user_routine_catalog_oid(&function)?)),
        ("tgtype", int_value(trigger_type(definition))),
        ("tgenabled", str_value(trigger.enabled.catalog_code())),
        ("tgisinternal", bool_value(false)),
        ("tgconstrrelid", int_value(referenced_relation_oid)),
        ("tgconstrindid", int_value(0)),
        ("tgconstraint", int_value(constraint_oid)),
        (
            "tgdeferrable",
            bool_value(definition.deferrability.is_deferrable()),
        ),
        (
            "tginitdeferred",
            bool_value(definition.deferrability.is_initially_deferred()),
        ),
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
        (
            "tgoldtable",
            definition
                .old_transition_table()
                .map_or(Value::Null, str_value),
        ),
        (
            "tgnewtable",
            definition
                .new_transition_table()
                .map_or(Value::Null, str_value),
        ),
    ]))
}

fn event_relation_columns(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &str,
) -> Result<Vec<(String, uqa_sql::ast::ColumnType)>, SQLError> {
    if let Some(table) = catalog.table_resolved(resolution, relation)? {
        return Ok(table
            .columns
            .iter()
            .map(|column| (column.name.clone(), column.ty.clone()))
            .collect());
    }
    if let Some(view) = catalog.view_resolved(resolution, relation)? {
        return Ok(view_columns_for(engine, catalog, resolution, view)?
            .into_iter()
            .map(|column| (column.name, column.ty))
            .collect());
    }
    if let Some(foreign) = catalog.foreign_table_resolved(resolution, relation)? {
        return Ok(foreign
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.clone(),
                    crate::engine_fdw::fdw_column_type_to_sql(&column.ty),
                )
            })
            .collect());
    }
    Err(SQLError::UnknownTable(relation.to_string()))
}

fn resolve_trigger_function(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<std::sync::Arc<SQLUserFunction>, SQLError> {
    let candidates = catalog
        .sql_functions(resolution, name)?
        .unwrap_or_default()
        .into_iter()
        .filter(|function| {
            !function.def.is_procedure && routine_signature_types(&function.def).is_empty()
        })
        .collect::<Vec<_>>();
    let function = match candidates.as_slice() {
        [function] => function.clone(),
        [] => {
            return Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!("function {name}() does not exist"),
            });
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42725".into(),
                message: format!("function name \"{name}\" is not unique"),
            });
        }
    };
    let returns_trigger = matches!(
        &function.def.returns,
        uqa_sql::ast::FunctionReturns::Scalar { type_name }
            if canonical_routine_type_name(type_name) == "trigger"
    );
    if !returns_trigger {
        return Err(SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!("function {} must return type trigger", function.def.name),
        });
    }
    if !matches!(function.compiled, CompiledFunctionBody::PLpgSQL(_)) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "only LANGUAGE plpgsql trigger functions are executable".into(),
        });
    }
    Ok(function)
}

pub(super) fn build_pg_rewrite(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = catalog
        .rules()
        .into_iter()
        .map(|rule| {
            let definition = &rule.definition;
            Ok(row([
                ("oid", int_value(rule_catalog_oid(&rule))),
                ("rulename", str_value(definition.name.clone())),
                (
                    "ev_class",
                    int_value(event_relation_oid_from(
                        catalog,
                        resolution,
                        &definition.table,
                    )?),
                ),
                ("ev_type", str_value(rule_event_code(definition.event))),
                ("ev_enabled", str_value(rule.enabled.catalog_code())),
                ("is_instead", bool_value(definition.instead)),
                (
                    "ev_qual",
                    rule_condition_text(definition, false)?
                        .map_or_else(|| str_value("<>"), str_value),
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
    for (name, view) in catalog.views_of_kind(crate::StoredViewKind::View) {
        rows.push(row([
            (
                "oid",
                int_value(stable_oid("rule", &format!("{name}._RETURN"))),
            ),
            ("rulename", str_value("_RETURN")),
            (
                "ev_class",
                int_value(event_relation_oid_from(catalog, resolution, &name)?),
            ),
            ("ev_type", str_value("1")),
            ("ev_enabled", str_value("O")),
            ("is_instead", bool_value(true)),
            ("ev_qual", str_value("<>")),
            ("ev_action", str_value(format!("{:?}", view.query))),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_pg_rules(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .rules()
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
                    str_value(render_rule_definition(
                        catalog, resolution, definition, false,
                    )?),
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
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    let mut found = None;
    for (trigger, _) in catalog_triggers(&catalog, &resolution)? {
        if trigger_catalog_oid(&catalog, &resolution, &trigger)? == oid {
            found = Some(trigger);
            break;
        }
    }
    let Some(trigger) = found else {
        return Ok(Value::Null);
    };
    Ok(str_value(render_trigger_definition(
        &catalog,
        &resolution,
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
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    if let Some(rule) = catalog
        .rules()
        .into_iter()
        .find(|rule| rule_catalog_oid(rule) == oid)
    {
        return Ok(str_value(render_rule_definition(
            &catalog,
            &resolution,
            &rule.definition,
            pretty,
        )?));
    }
    for (name, _) in catalog.views_of_kind(crate::StoredViewKind::View) {
        if stable_oid("rule", &format!("{name}._RETURN")) == oid {
            return Ok(str_value(format!(
                "CREATE RULE \"_RETURN\" AS ON SELECT TO {} DO INSTEAD SELECT ...",
                render_rule_relation(&catalog, &resolution, &name, pretty)?
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
    match definition.timing {
        TriggerTiming::Before => value |= TRIGGER_TYPE_BEFORE,
        TriggerTiming::InsteadOf => value |= TRIGGER_TYPE_INSTEAD,
        TriggerTiming::After => {}
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
fn render_trigger_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    definition: &CreateTrigger,
    pretty: bool,
) -> Result<String, SQLError> {
    let events = [
        TriggerEvent::Insert,
        TriggerEvent::Delete,
        TriggerEvent::Update,
        TriggerEvent::Truncate,
    ]
    .into_iter()
    .filter(|event| definition.events.contains(event))
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
        "CREATE {}TRIGGER {} {} {} ON {}",
        if definition.constraint {
            "CONSTRAINT "
        } else {
            ""
        },
        uqa_sql::expr::quote_ident(&definition.name),
        match definition.timing {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
            TriggerTiming::InsteadOf => "INSTEAD OF",
        },
        events,
        render_trigger_relation(catalog, resolution, &definition.table, pretty)?,
    );
    if let Some(referenced_table) = definition.referenced_table.as_deref() {
        rendered.push_str(" FROM ");
        // PostgreSQL deparses the constraint trigger's FROM relation with
        // visibility-based qualification even in the non-pretty form.
        rendered.push_str(&render_trigger_relation(
            catalog,
            resolution,
            referenced_table,
            true,
        )?);
    }
    if definition.constraint {
        rendered.push_str(if definition.deferrability.is_deferrable() {
            " DEFERRABLE"
        } else {
            " NOT DEFERRABLE"
        });
        rendered.push_str(if definition.deferrability.is_initially_deferred() {
            " INITIALLY DEFERRED"
        } else {
            " INITIALLY IMMEDIATE"
        });
    }
    if !definition.transition_relations.is_empty() {
        rendered.push_str(" REFERENCING ");
        rendered.push_str(
            &definition
                .transition_relations
                .iter()
                .filter(|relation| !relation.is_new)
                .chain(
                    definition
                        .transition_relations
                        .iter()
                        .filter(|relation| relation.is_new),
                )
                .map(|relation| {
                    format!(
                        "{} {} AS {}",
                        if relation.is_new { "NEW" } else { "OLD" },
                        if relation.is_table { "TABLE" } else { "ROW" },
                        uqa_sql::expr::quote_ident(&relation.name)
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    rendered.push_str(" FOR EACH ");
    rendered.push_str(if definition.row { "ROW" } else { "STATEMENT" });
    if let Some(condition) = &definition.when {
        rendered.push_str(" WHEN (");
        rendered.push_str(&render_trigger_condition(condition, pretty));
        rendered.push(')');
    }
    rendered.push_str(" EXECUTE FUNCTION ");
    rendered.push_str(&render_trigger_function(
        catalog,
        resolution,
        &definition.function,
    ));
    rendered.push('(');
    rendered.push_str(&arguments);
    rendered.push(')');
    Ok(rendered)
}

fn render_trigger_relation(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    pretty: bool,
) -> Result<String, SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("decode trigger relation `{name}`: {error}"))
    })?;
    if pretty {
        let local = uqa_sql::expr::quote_ident(&relation.name);
        let visible_table = catalog.table_name_resolved(resolution, &local)?;
        let visible_view = catalog.view_name_resolved(resolution, &local)?;
        if visible_table.as_deref() == Some(name) || visible_view.as_deref() == Some(name) {
            return Ok(local);
        }
    }
    Ok(render_qualified_name(name))
}

fn render_trigger_function(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> String {
    let Ok(function) = RelationIdentity::from_legacy_name(name) else {
        return render_qualified_name(name);
    };
    let local = uqa_sql::expr::quote_ident(&function.name);
    if resolve_trigger_function(catalog, resolution, &local)
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
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
            Expr::Func { binding, args, .. }
                if binding.as_ref().and_then(|binding| binding.dispatch)
                    == Some(FunctionDispatch::IsDistinct)
                    && args.len() == 2 =>
            {
                (
                    4,
                    format!(
                        "{} IS NOT DISTINCT FROM {}",
                        render_pretty_expr(&args[0], 5),
                        render_pretty_expr(&args[1], 5)
                    ),
                )
            }
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
        Expr::Func { binding, args, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(FunctionDispatch::IsDistinct)
                && args.len() == 2 =>
        {
            (
                4,
                format!(
                    "{} IS DISTINCT FROM {}",
                    render_pretty_expr(&args[0], 5),
                    render_pretty_expr(&args[1], 5)
                ),
            )
        }
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
