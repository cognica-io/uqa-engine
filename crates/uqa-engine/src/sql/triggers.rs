//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `BEFORE`/`AFTER`, row-level, and statement-level trigger execution.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Value};
use uqa_sql::ast::{TriggerEvent, TriggerTiming};
use uqa_sql::error::Result;
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

use super::coerce_to_column_type;
use super::plpgsql_exec::{execute_trigger_routine, TriggerRoutineContext};
use super::scalar::eval_lowered_expression;

struct TriggerVariableResolver<'a> {
    old: &'a Value,
    new: &'a Value,
    types: &'a BTreeMap<String, String>,
}

impl TriggerVariableResolver<'_> {
    fn record_field(&self, record: &Value, column: &str) -> Result<ResolvedVariable> {
        let value = match record {
            Value::Null => Value::Null,
            Value::Record(fields) => fields
                .iter()
                .find(|(name, _)| name == column)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?,
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "55000".into(),
                    message: "trigger row variable is not assigned yet".into(),
                })
            }
        };
        Ok(ResolvedVariable {
            value,
            declared_type: self.types.get(column).cloned(),
        })
    }
}

impl VariableResolver for TriggerVariableResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self.record_field(self.old, column).map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self.record_field(self.new, column).map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>> {
        Ok(None)
    }
}

fn trigger_column_types(engine: &Engine, table: &str) -> Result<BTreeMap<String, String>> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read trigger row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok(columns
        .into_iter()
        .map(|column| (column.name, column.ty.sql_name()))
        .collect())
}

fn trigger_record(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    document: Option<&Document>,
    mask_generated: bool,
) -> Result<Value> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read trigger row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let Some(document) = document else {
        return Ok(Value::Record(
            definitions
                .iter()
                .map(|column| (column.name.clone(), Value::Null))
                .collect(),
        ));
    };
    let mut materialized = document.clone();
    for column in &definitions {
        let unavailable = column.generated.as_ref().is_some_and(|generated| {
            mask_generated || generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
        });
        if unavailable {
            materialized.insert(column.name.clone(), Value::Null);
        }
    }
    if definitions.is_empty() {
        return Ok(Value::Record(materialized.into_iter().collect()));
    }
    let fallback_id = i64::try_from(doc_id).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("document id {doc_id} exceeds PostgreSQL bigint"))
    })?;
    Ok(Value::Record(
        definitions
            .iter()
            .map(|column| {
                let value = materialized.get(&column.name).cloned().unwrap_or_else(|| {
                    if column.primary_key && column.ty.is_integer() {
                        fallback_id.clone()
                    } else {
                        Value::Null
                    }
                });
                (column.name.clone(), value)
            })
            .collect(),
    ))
}

fn trigger_document(engine: &Engine, table: &str, value: Value) -> Result<Option<Document>> {
    let fields = match value {
        Value::Null => return Ok(None),
        Value::Record(fields) => fields,
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "39P01".into(),
                message: "trigger function returned non-composite value".into(),
            })
        }
    };
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read trigger row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    if definitions.is_empty() {
        return Ok(Some(fields.into_iter().collect()));
    }
    let known = definitions
        .iter()
        .map(|column| column.name.as_str())
        .collect::<BTreeSet<_>>();
    if let Some((unknown, _)) = fields
        .iter()
        .find(|(name, _)| !known.contains(name.as_str()))
    {
        return Err(SQLError::UnknownColumn(format!("{table}.{unknown}")));
    }
    let values = fields.into_iter().collect::<BTreeMap<_, _>>();
    let mut document = Document::new();
    for column in definitions {
        let value = values.get(&column.name).cloned().unwrap_or(Value::Null);
        document.insert(
            column.name.clone(),
            coerce_to_column_type(engine, table, &column.name, value)?,
        );
    }
    Ok(Some(document))
}

fn operation_name(event: TriggerEvent) -> &'static str {
    match event {
        TriggerEvent::Insert => "INSERT",
        TriggerEvent::Update => "UPDATE",
        TriggerEvent::Delete => "DELETE",
        TriggerEvent::Truncate => "TRUNCATE",
    }
}

fn timing_name(timing: TriggerTiming) -> &'static str {
    match timing {
        TriggerTiming::Before => "BEFORE",
        TriggerTiming::After => "AFTER",
    }
}

fn trigger_condition_matches(
    engine: &Engine,
    condition: Option<&uqa_sql::ast::Expr>,
    old: &Value,
    new: &Value,
    types: &BTreeMap<String, String>,
) -> Result<bool> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = bind_expr(condition, &mut TriggerVariableResolver { old, new, types })?;
    Ok(uqa_sql::expr::truthy(&eval_lowered_expression(
        engine,
        &condition,
        None,
        &[],
    )?))
}

struct TriggerInvocation<'a> {
    table: &'a str,
    trigger: &'a crate::engine_events::StoredTrigger,
    timing: TriggerTiming,
    event: TriggerEvent,
    row: bool,
    old: Value,
    new: Value,
}

fn invoke_trigger(engine: &Engine, invocation: TriggerInvocation<'_>) -> Result<Value> {
    let relation = RelationIdentity::from_legacy_name(invocation.table).map_err(|error| {
        SQLError::Internal(format!(
            "decode trigger relation `{}`: {error}",
            invocation.table
        ))
    })?;
    let function = engine.resolve_trigger_function(&invocation.trigger.definition.function)?;
    execute_trigger_routine(
        engine,
        &function,
        &TriggerRoutineContext {
            old: invocation.old,
            new: invocation.new,
            name: invocation.trigger.definition.name.clone(),
            when: timing_name(invocation.timing).into(),
            level: if invocation.row { "ROW" } else { "STATEMENT" }.into(),
            operation: operation_name(invocation.event).into(),
            relation_oid: super::catalog::table_relation_oid(engine, invocation.table)?,
            table_name: relation.name,
            table_schema: relation.schema,
            arguments: invocation.trigger.definition.arguments.clone(),
        },
    )
}

pub(super) fn fire_statement_triggers(
    engine: &Engine,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<()> {
    for trigger in engine.triggers_for(table, timing, event, false, updated_columns)? {
        let _ = invoke_trigger(
            engine,
            TriggerInvocation {
                table,
                trigger: &trigger,
                timing,
                event,
                row: false,
                old: Value::Null,
                new: Value::Null,
            },
        )?;
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct ReferentialTriggerStatements {
    seen: BTreeSet<String>,
    after: Vec<ReferentialStatementTrigger>,
}

struct ReferentialStatementTrigger {
    table: String,
    event: TriggerEvent,
    updated_columns: Vec<String>,
}

impl ReferentialTriggerStatements {
    pub(super) fn begin(
        &mut self,
        engine: &Engine,
        identity: String,
        table: &str,
        event: TriggerEvent,
        updated_columns: &[String],
    ) -> Result<()> {
        if !self.seen.insert(identity) {
            return Ok(());
        }
        fire_statement_triggers(engine, table, TriggerTiming::Before, event, updated_columns)?;
        self.after.push(ReferentialStatementTrigger {
            table: table.to_string(),
            event,
            updated_columns: updated_columns.to_vec(),
        });
        Ok(())
    }

    pub(super) fn fire_after(&self, engine: &Engine) -> Result<()> {
        for statement in &self.after {
            fire_statement_triggers(
                engine,
                &statement.table,
                TriggerTiming::After,
                statement.event,
                &statement.updated_columns,
            )?;
        }
        Ok(())
    }
}

pub(super) fn fire_before_row_triggers(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    doc_id: DocId,
    old_document: Option<&Document>,
    new_document: Option<&Document>,
    updated_columns: &[String],
) -> Result<Option<Document>> {
    let types = trigger_column_types(engine, table)?;
    let old = trigger_record(engine, table, doc_id, old_document, false)?;
    let mut new = trigger_record(engine, table, doc_id, new_document, true)?;
    for trigger in
        engine.triggers_for(table, TriggerTiming::Before, event, true, updated_columns)?
    {
        if !trigger_condition_matches(engine, trigger.definition.when.as_ref(), &old, &new, &types)?
        {
            continue;
        }
        let returned = invoke_trigger(
            engine,
            TriggerInvocation {
                table,
                trigger: &trigger,
                timing: TriggerTiming::Before,
                event,
                row: true,
                old: old.clone(),
                new: new.clone(),
            },
        )?;
        if matches!(returned, Value::Null) {
            return Ok(None);
        }
        if event != TriggerEvent::Delete {
            new = returned;
        }
    }
    if event == TriggerEvent::Delete {
        return Ok(old_document.cloned());
    }
    trigger_document(engine, table, new)
}

fn fire_after_row_trigger(engine: &Engine, event: &AfterRowTriggerEvent) -> Result<()> {
    for trigger in &event.triggers {
        if trigger.definition.constraint && engine.constraint_trigger_is_deferred(trigger)? {
            engine.defer_constraint_trigger_event(DeferredConstraintTriggerEvent {
                constraint: Engine::constraint_trigger_identity(trigger)?,
                firing_relation: RelationIdentity::from_legacy_name(&event.table).map_err(
                    |error| {
                        SQLError::Internal(format!(
                            "decode deferred trigger relation `{}`: {error}",
                            event.table
                        ))
                    },
                )?,
                table: event.table.clone(),
                event: event.event,
                old: event.old.clone(),
                new: event.new.clone(),
                trigger: trigger.clone(),
            })?;
            continue;
        }
        let _ = invoke_trigger(
            engine,
            TriggerInvocation {
                table: &event.table,
                trigger,
                timing: TriggerTiming::After,
                event: event.event,
                row: true,
                old: event.old.clone(),
                new: event.new.clone(),
            },
        )?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct DeferredConstraintTriggerEvent {
    pub(crate) constraint: crate::ConstraintIdentity,
    pub(crate) firing_relation: RelationIdentity,
    pub(crate) table: String,
    event: TriggerEvent,
    old: Value,
    new: Value,
    pub(crate) trigger: crate::engine_events::StoredTrigger,
}

pub(crate) fn fire_deferred_constraint_trigger_event(
    engine: &Engine,
    event: &DeferredConstraintTriggerEvent,
) -> Result<()> {
    let _ = invoke_trigger(
        engine,
        TriggerInvocation {
            table: &event.table,
            trigger: &event.trigger,
            timing: TriggerTiming::After,
            event: event.event,
            row: true,
            old: event.old.clone(),
            new: event.new.clone(),
        },
    )?;
    Ok(())
}

pub(super) struct AfterRowTriggerEvent {
    table: String,
    event: TriggerEvent,
    old: Value,
    new: Value,
    triggers: Vec<crate::engine_events::StoredTrigger>,
}

pub(super) struct AfterRowTriggerInput<'a> {
    pub(super) table: &'a str,
    pub(super) event: TriggerEvent,
    pub(super) old_doc_id: DocId,
    pub(super) new_doc_id: DocId,
    pub(super) old_document: Option<&'a Document>,
    pub(super) new_document: Option<&'a Document>,
    pub(super) updated_columns: &'a [String],
}

impl AfterRowTriggerEvent {
    pub(super) fn prepare(
        engine: &Engine,
        input: AfterRowTriggerInput<'_>,
    ) -> Result<Option<Self>> {
        let AfterRowTriggerInput {
            table,
            event,
            old_doc_id,
            new_doc_id,
            old_document,
            new_document,
            updated_columns,
        } = input;
        let candidates =
            engine.triggers_for(table, TriggerTiming::After, event, true, updated_columns)?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let types = trigger_column_types(engine, table)?;
        let old = trigger_record(engine, table, old_doc_id, old_document, false)?;
        let new = trigger_record(engine, table, new_doc_id, new_document, false)?;
        let mut matching = Vec::new();
        for trigger in candidates {
            if trigger_condition_matches(
                engine,
                trigger.definition.when.as_ref(),
                &old,
                &new,
                &types,
            )? {
                matching.push(trigger);
            }
        }
        if matching.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            table: table.to_string(),
            event,
            old,
            new,
            triggers: matching,
        }))
    }
}

pub(super) fn fire_after_row_trigger_events(
    engine: &Engine,
    events: &[AfterRowTriggerEvent],
) -> Result<()> {
    for event in events {
        fire_after_row_trigger(engine, event)?;
    }
    Ok(())
}
