//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `BEFORE`/`AFTER`, row-level, and statement-level trigger execution.

use std::cell::RefCell;
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

thread_local! {
    static ACTIVE_TRANSITION_RELATIONS: RefCell<Vec<BTreeMap<String, uqa_execution::SharedSpill>>> = const { RefCell::new(Vec::new()) };
}

struct TransitionRelationScope;

impl Drop for TransitionRelationScope {
    fn drop(&mut self) {
        ACTIVE_TRANSITION_RELATIONS.with(|relations| {
            let removed = relations.borrow_mut().pop();
            debug_assert!(
                removed.is_some(),
                "transition relation scope stack underflow"
            );
        });
    }
}

pub(super) fn current_transition_relations() -> BTreeMap<String, uqa_execution::SharedSpill> {
    ACTIVE_TRANSITION_RELATIONS
        .with(|relations| relations.borrow().last().cloned().unwrap_or_default())
}

pub(super) fn current_transition_relation_names() -> BTreeSet<String> {
    ACTIVE_TRANSITION_RELATIONS.with(|relations| {
        relations
            .borrow()
            .last()
            .map(|relations| relations.keys().cloned().collect())
            .unwrap_or_default()
    })
}

pub(super) struct TransitionTables {
    root_table: String,
    event: TriggerEvent,
    source_tables: BTreeSet<String>,
    old: Option<uqa_execution::SharedSpill>,
    new: Option<uqa_execution::SharedSpill>,
}

impl TransitionTables {
    fn applies_to(&self, event: &AfterRowTriggerEvent) -> bool {
        self.event == event.event && self.source_tables.contains(&event.table)
    }

    fn matches_statement(&self, table: &str, event: TriggerEvent) -> bool {
        self.event == event && self.root_table == table
    }

    fn enter(
        &self,
        definition: &uqa_sql::ast::CreateTrigger,
    ) -> Result<Option<TransitionRelationScope>> {
        if definition.transition_relations.is_empty() {
            return Ok(None);
        }
        let mut relations = BTreeMap::new();
        if let Some(name) = definition.old_transition_table() {
            let rows = self.old.as_ref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "trigger `{}` requested an unavailable OLD transition table",
                    definition.name
                ))
            })?;
            relations.insert(name.to_string(), rows.clone());
        }
        if let Some(name) = definition.new_transition_table() {
            let rows = self.new.as_ref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "trigger `{}` requested an unavailable NEW transition table",
                    definition.name
                ))
            })?;
            relations.insert(name.to_string(), rows.clone());
        }
        ACTIVE_TRANSITION_RELATIONS.with(|active| active.borrow_mut().push(relations));
        Ok(Some(TransitionRelationScope))
    }
}

fn materialize_transition_rows(
    engine: &Engine,
    table: &str,
    values: impl IntoIterator<Item = Value>,
) -> Result<uqa_execution::SharedSpill> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read transition row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let names = columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let schema = uqa_execution::RowSchema::with_types(
        names.clone(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let mut spill = uqa_execution::SpillBuffer::new(
        crate::sql::select::physical_work_mem_bytes(engine)?.max(1),
    );
    let mut rows = Vec::with_capacity(uqa_execution::DEFAULT_BATCH_SIZE);
    for value in values {
        let Value::Record(fields) = value else {
            return Err(SQLError::Internal(
                "transition relation row is not a record".into(),
            ));
        };
        let fields = fields.into_iter().collect::<BTreeMap<_, _>>();
        let mut row = uqa_sql::ResultRow::new();
        for name in &names {
            row.insert(
                name.clone(),
                fields.get(name).cloned().unwrap_or(Value::Null),
            );
        }
        rows.push(row);
        if rows.len() == uqa_execution::DEFAULT_BATCH_SIZE {
            spill
                .push(uqa_execution::Batch::new(
                    schema.clone(),
                    std::mem::take(&mut rows),
                ))
                .map_err(crate::sql::select::physical_exec_error)?;
            rows.reserve(uqa_execution::DEFAULT_BATCH_SIZE);
        }
    }
    if !rows.is_empty() {
        spill
            .push(uqa_execution::Batch::new(schema.clone(), rows))
            .map_err(crate::sql::select::physical_exec_error)?;
    }
    spill
        .into_shared(schema)
        .map_err(crate::sql::select::physical_exec_error)
}

pub(super) fn transition_capture_required(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<bool> {
    let canonical = engine
        .try_resolve_table_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve transition source: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut pending = vec![canonical];
    let mut visited = BTreeSet::new();
    while let Some(source) = pending.pop() {
        if !visited.insert(source.clone()) {
            continue;
        }
        for row in [false, true] {
            if engine
                .triggers_for(&source, TriggerTiming::After, event, row, updated_columns)?
                .iter()
                .any(|trigger| !trigger.definition.transition_relations.is_empty())
            {
                return Ok(true);
            }
        }
        let hierarchy = engine.try_table_hierarchy(&source).map_err(|error| {
            SQLError::Internal(format!("read transition source hierarchy: {error}"))
        })?;
        pending.extend(hierarchy.parents);
    }
    Ok(false)
}

pub(super) fn build_transition_tables(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    events: &[AfterRowTriggerEvent],
) -> Result<Option<TransitionTables>> {
    let mut triggers =
        engine.triggers_for(table, TriggerTiming::After, event, false, updated_columns)?;
    triggers.extend(engine.triggers_for(
        table,
        TriggerTiming::After,
        event,
        true,
        updated_columns,
    )?);
    triggers.retain(|trigger| !trigger.definition.transition_relations.is_empty());
    if triggers.is_empty() {
        return Ok(None);
    }
    let source_tables = engine
        .hierarchy_scan_tables(table, true)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let matching = events
        .iter()
        .filter(|candidate| candidate.event == event && source_tables.contains(&candidate.table))
        .collect::<Vec<_>>();
    let need_old = triggers
        .iter()
        .any(|trigger| trigger.definition.old_transition_table().is_some());
    let need_new = triggers
        .iter()
        .any(|trigger| trigger.definition.new_transition_table().is_some());
    let old = need_old
        .then(|| {
            materialize_transition_rows(
                engine,
                table,
                matching.iter().map(|candidate| candidate.old.clone()),
            )
        })
        .transpose()?;
    let new = need_new
        .then(|| {
            materialize_transition_rows(
                engine,
                table,
                matching.iter().map(|candidate| candidate.new.clone()),
            )
        })
        .transpose()?;
    Ok(Some(TransitionTables {
        root_table: table.to_string(),
        event,
        source_tables,
        old,
        new,
    }))
}

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

fn invoke_trigger(
    engine: &Engine,
    invocation: TriggerInvocation<'_>,
    transition_tables: Option<&TransitionTables>,
) -> Result<Value> {
    let relation = RelationIdentity::from_legacy_name(invocation.table).map_err(|error| {
        SQLError::Internal(format!(
            "decode trigger relation `{}`: {error}",
            invocation.table
        ))
    })?;
    let function = engine.resolve_trigger_function(&invocation.trigger.definition.function)?;
    let _transition_scope = transition_tables
        .map(|tables| tables.enter(&invocation.trigger.definition))
        .transpose()?
        .flatten();
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
    fire_statement_triggers_with_transition(engine, table, timing, event, updated_columns, None)
}

fn fire_statement_triggers_with_transition(
    engine: &Engine,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: Option<&TransitionTables>,
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
            transition_tables,
        )?;
    }
    Ok(())
}

pub(super) fn fire_after_statement_triggers(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: Option<&TransitionTables>,
) -> Result<()> {
    fire_statement_triggers_with_transition(
        engine,
        table,
        TriggerTiming::After,
        event,
        updated_columns,
        transition_tables,
    )
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
        if let Some(statement) = self
            .after
            .iter_mut()
            .find(|statement| statement.table == table && statement.event == event)
        {
            for column in updated_columns {
                if !statement.updated_columns.contains(column) {
                    statement.updated_columns.push(column.clone());
                }
            }
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

    pub(super) fn build_transition_tables(
        &self,
        engine: &Engine,
        events: &[AfterRowTriggerEvent],
    ) -> Result<Vec<TransitionTables>> {
        let mut tables = Vec::new();
        for statement in &self.after {
            if let Some(transition) = build_transition_tables(
                engine,
                &statement.table,
                statement.event,
                &statement.updated_columns,
                events,
            )? {
                tables.push(transition);
            }
        }
        Ok(tables)
    }

    pub(super) fn fire_after(
        &self,
        engine: &Engine,
        transitions: &[TransitionTables],
    ) -> Result<()> {
        for statement in &self.after {
            let transition = transitions
                .iter()
                .find(|transition| transition.matches_statement(&statement.table, statement.event));
            fire_after_statement_triggers(
                engine,
                &statement.table,
                statement.event,
                &statement.updated_columns,
                transition,
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
            None,
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

fn fire_after_row_trigger(
    engine: &Engine,
    event: &AfterRowTriggerEvent,
    transition_tables: &[&TransitionTables],
) -> Result<()> {
    let transition_tables = transition_tables
        .iter()
        .find(|tables| tables.applies_to(event))
        .copied();
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
            transition_tables,
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
        None,
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
        let capture_transition =
            transition_capture_required(engine, table, event, updated_columns)?;
        if candidates.is_empty() && !capture_transition {
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
        if matching.is_empty() && !capture_transition {
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

pub(super) fn fire_after_row_trigger_events_with_transitions(
    engine: &Engine,
    events: &[AfterRowTriggerEvent],
    transition_tables: &[&TransitionTables],
) -> Result<()> {
    for event in events {
        fire_after_row_trigger(engine, event, transition_tables)?;
    }
    Ok(())
}
