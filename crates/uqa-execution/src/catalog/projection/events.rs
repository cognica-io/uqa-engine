//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_trigger`, `pg_rewrite`, and their definition helpers.

mod rules;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{CreateTrigger, RuleEvent, TriggerEvent, TriggerTiming};
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::canonical_routine_type_name;
use uqa_sql::catalog::events::{StoredRule, StoredTrigger};
use uqa_sql::routines::{routine_signature_types, SQLUserFunction};

use super::helpers::oids::{namespace_oid, split_schema_name, stable_oid};
use super::helpers::rows::{bool_value, catalog_usize, int_value, row, str_value};
use super::helpers::views::view_columns_for;
use super::pg_catalog::table_relation_oid_from;
use super::pg_proc::user_routine_catalog_oid;

use rules::{render_rule_definition, render_rule_relation};

const TRIGGER_TYPE_ROW: i64 = 1;
const TRIGGER_TYPE_BEFORE: i64 = 2;
const TRIGGER_TYPE_INSERT: i64 = 4;
const TRIGGER_TYPE_DELETE: i64 = 8;
const TRIGGER_TYPE_UPDATE: i64 = 16;
const TRIGGER_TYPE_TRUNCATE: i64 = 32;
const TRIGGER_TYPE_INSTEAD: i64 = 64;

pub fn event_relation_oid(context: &CatalogContext<'_>, relation: &str) -> Result<i64, SQLError> {
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
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
    if let Some((_, table)) = catalog.foreign_table_entry_resolved(resolution, relation)? {
        return Ok(super::foreign_table_relation_oid(&table));
    }
    let view = catalog
        .view_resolved(resolution, relation)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    Ok(super::view_relation_oid(view))
}

pub fn trigger_catalog_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: &StoredTrigger,
) -> Result<i64, SQLError> {
    if let Some(oid) = trigger.catalog_oid {
        return Ok(oid);
    }
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

pub fn trigger_constraint_catalog_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: &StoredTrigger,
) -> Result<i64, SQLError> {
    if !trigger.definition.constraint {
        return Ok(0);
    }
    if let Some(oid) = trigger.constraint_catalog_oid {
        return Ok(oid);
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

pub fn rule_catalog_oid(rule: &StoredRule) -> i64 {
    rule.catalog_oid
        .unwrap_or_else(|| legacy_rule_catalog_oid(rule))
}

/// The OID a rule created before OIDs were recorded derives from its relation and name.
pub fn legacy_rule_catalog_oid(rule: &StoredRule) -> i64 {
    stable_oid(
        "rule",
        &format!("{}.{}", rule.definition.table, rule.definition.name),
    )
}

pub fn build_pg_trigger(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    catalog_triggers(catalog, resolution)?
        .into_iter()
        .map(|(trigger, parent_oid)| {
            pg_trigger_row(context, catalog, resolution, trigger, parent_oid)
        })
        .collect()
}

pub fn catalog_triggers(
    catalog_view: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<(StoredTrigger, i64)>, SQLError> {
    // Catalog metadata names its relations canonically, independently of invoking-role visibility.
    let mut resolution = resolution.clone();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let resolution = &resolution;
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
                // A partition's clone of a row trigger has OIDs of its own, derived from the trigger's identity and the partition; the recorded OIDs are those of the trigger on its own table.
                let mut clone = original.clone();
                clone.definition.table.clone_from(&table);
                clone.catalog_oid = None;
                clone.constraint_catalog_oid = None;
                let mut parent_clone = original.clone();
                parent_clone.definition.table = parent.qualified_name();
                if *parent != *source {
                    parent_clone.catalog_oid = None;
                    parent_clone.constraint_catalog_oid = None;
                }
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

pub fn build_trigger_constraints(
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
            (
                "connamespace",
                int_value(namespace_oid(catalog, &relation.schema)),
            ),
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
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    trigger: StoredTrigger,
    parent_oid: i64,
) -> Result<ResultRow, SQLError> {
    let definition = &trigger.definition;
    let function = resolve_trigger_function(catalog, resolution, &definition.function)?;
    let columns = event_relation_columns(context, catalog, resolution, &definition.table)?;
    let attributes = definition
        .update_columns
        .iter()
        .map(|name| {
            columns
                .iter()
                .find(|(_, column)| column == name)
                .ok_or_else(|| SQLError::UnknownColumn(name.clone()))
                .map(|(number, _)| i64::from(*number))
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
        (
            "tgattr",
            super::helpers::rows::catalog_int2vector(attributes, "pg_trigger.tgattr")?,
        ),
        ("tgargs", Value::Bytes(arguments)),
        (
            "tgqual",
            match definition.when.as_ref() {
                Some(condition) => str_value(super::view_definition::stored_expression_text(
                    Some(&crate::catalog::projection::CatalogOutput(*context)),
                    catalog,
                    resolution,
                    condition,
                )?),
                None => Value::Null,
            },
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
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &str,
) -> Result<Vec<(i16, String)>, SQLError> {
    if let Some(table) = catalog.table_resolved(resolution, relation)? {
        return uqa_sql::catalog::relation_attributes::column_names(&table.columns);
    }
    if let Some(view) = catalog.view_resolved(resolution, relation)? {
        return uqa_sql::catalog::relation_attributes::column_names(&view_columns_for(
            context, catalog, resolution, view,
        )?);
    }
    if let Some(foreign) = catalog.foreign_table_resolved(resolution, relation)? {
        return uqa_sql::catalog::relation_attributes::column_names(&foreign.columns);
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
    if function.def.language != "plpgsql" {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "only LANGUAGE plpgsql trigger functions are executable".into(),
        });
    }
    Ok(function)
}

/// Rule addresses without rendering stored conditions or serializing actions.
pub(super) fn rewrite_catalog_oids(catalog: &CatalogReadView) -> Vec<i64> {
    catalog
        .rules()
        .iter()
        .map(rule_catalog_oid)
        .chain(catalog_view_rules(catalog).map(|(_, view)| view_rule_oid(&view)))
        .collect()
}

pub fn build_pg_rewrite(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
                    match definition.condition.as_ref() {
                        Some(condition) => {
                            str_value(super::view_definition::stored_expression_text(
                                output, catalog, resolution, condition,
                            )?)
                        }
                        None => str_value("<>"),
                    },
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
    for (name, view) in catalog_view_rules(catalog) {
        rows.push(row([
            ("oid", int_value(view_rule_oid(&view))),
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

pub fn build_pg_rules(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
                        output, catalog, resolution, definition, false,
                    )?),
                ),
            ]))
        })
        .collect()
}

pub fn pg_get_triggerdef_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let definition_arguments = definition_arguments("pg_get_triggerdef", arguments)?;
    let Some((oid, pretty)) = definition_arguments else {
        return Ok(Value::Null);
    };
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let mut lookup = resolution.clone();
    lookup.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let mut found = None;
    for (trigger, _) in catalog_triggers(&catalog, &lookup)? {
        if trigger_catalog_oid(&catalog, &lookup, &trigger)? == oid {
            found = Some(trigger);
            break;
        }
    }
    let Some(trigger) = found else {
        return Ok(Value::Null);
    };
    Ok(str_value(render_trigger_definition(
        Some(&crate::catalog::projection::CatalogOutput(*context)),
        &catalog,
        &resolution,
        &trigger.definition,
        pretty,
    )?))
}

pub fn pg_get_ruledef_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let definition_arguments = definition_arguments("pg_get_ruledef", arguments)?;
    let Some((oid, pretty)) = definition_arguments else {
        return Ok(Value::Null);
    };
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    if let Some(rule) = catalog
        .rules()
        .into_iter()
        .find(|rule| rule_catalog_oid(rule) == oid)
    {
        return Ok(str_value(render_rule_definition(
            Some(&crate::catalog::projection::CatalogOutput(*context)),
            &catalog,
            &resolution,
            &rule.definition,
            pretty,
        )?));
    }
    for (name, view) in catalog_view_rules(&catalog) {
        if view_rule_oid(&view) == oid {
            let query = super::view_definition::view_definition(
                Some(&crate::catalog::projection::CatalogOutput(*context)),
                &catalog,
                &resolution,
                &view,
                pretty,
                0,
            )?;
            return Ok(str_value(format!(
                "CREATE RULE \"_RETURN\" AS\n    ON SELECT TO {} DO INSTEAD {query}",
                render_rule_relation(&catalog, &resolution, &name, pretty)?
            )));
        }
    }
    Ok(Value::Null)
}

fn catalog_view_rules(
    catalog: &CatalogReadView,
) -> impl Iterator<Item = (String, crate::catalog::view::StoredView)> + '_ {
    [
        crate::catalog::view::StoredViewKind::View,
        crate::catalog::view::StoredViewKind::Materialized,
    ]
    .into_iter()
    .flat_map(|kind| catalog.views_of_kind(kind))
}

fn view_rule_oid(view: &crate::catalog::view::StoredView) -> i64 {
    i64::from(
        view.relation_oids()
            .rule
            .expect("a view's OIDs include its _RETURN rule"),
    )
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
    output: Option<&dyn uqa_sql::expr::EngineHook>,
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
        rendered.push_str(&super::view_definition::trigger_condition_definition(
            output, catalog, resolution, condition, pretty,
        )?);
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

fn render_qualified_name(name: &str) -> String {
    name.split('.')
        .map(uqa_sql::expr::quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}
