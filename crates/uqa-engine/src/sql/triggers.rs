//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `BEFORE`/`AFTER`, row-level, and statement-level trigger execution.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uqa_core::{DocId, Value};
use uqa_sql::ast::{ForeignKeyAction, TriggerEvent, TriggerTiming};
use uqa_sql::error::Result;
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

use super::coerce_to_column_type;
use super::plpgsql_exec::{execute_trigger_routine, TriggerRoutineContext};
use super::scalar::eval_lowered_expression;

type TransitionCaptureKey = (String, &'static str, Vec<String>);
type TransitionCaptureStack = Vec<BTreeMap<TransitionCaptureKey, bool>>;

thread_local! {
    static ACTIVE_TRANSITION_RELATIONS: RefCell<Vec<BTreeMap<String, uqa_execution::SharedSpill>>> = const { RefCell::new(Vec::new()) };
    static TRANSITION_CAPTURE_CACHE: RefCell<TransitionCaptureStack> = const { RefCell::new(Vec::new()) };
}

pub(in crate::sql) struct TransitionRelationScope;

impl TransitionRelationScope {
    fn enter(relations: BTreeMap<String, uqa_execution::SharedSpill>) -> Self {
        ACTIVE_TRANSITION_RELATIONS.with(|active| active.borrow_mut().push(relations));
        Self
    }

    fn empty() -> Self {
        Self::enter(BTreeMap::new())
    }
}

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

pub(in crate::sql) fn enter_empty_transition_relation_scope() -> TransitionRelationScope {
    TransitionRelationScope::empty()
}

pub(in crate::sql) struct TransitionCaptureScope;

impl TransitionCaptureScope {
    pub(in crate::sql) fn enter() -> Self {
        TRANSITION_CAPTURE_CACHE.with(|cache| cache.borrow_mut().push(BTreeMap::new()));
        Self
    }
}

impl Drop for TransitionCaptureScope {
    fn drop(&mut self) {
        TRANSITION_CAPTURE_CACHE.with(|cache| {
            let removed = cache.borrow_mut().pop();
            debug_assert!(
                removed.is_some(),
                "transition capture cache stack underflow"
            );
        });
    }
}

pub(super) struct TransitionTables {
    root_table: String,
    event: TriggerEvent,
    generation: usize,
    event_generations: BTreeMap<usize, usize>,
    event_order: BTreeMap<usize, usize>,
    source_tables: BTreeSet<String>,
    old: Option<uqa_execution::SharedSpill>,
    new: Option<uqa_execution::SharedSpill>,
}

impl TransitionTables {
    fn covers(&self, event: &AfterRowTriggerEvent) -> bool {
        self.event == event.event && self.source_tables.contains(&event.table)
    }

    fn applies_to(&self, event: &AfterRowTriggerEvent) -> bool {
        self.covers(event) && self.event_generations.get(&event.sequence) == Some(&self.generation)
    }

    fn generation_for(&self, event: &AfterRowTriggerEvent) -> Option<usize> {
        self.covers(event)
            .then(|| self.event_generations.get(&event.sequence).copied())
            .flatten()
    }

    fn order_for(&self, event: &AfterRowTriggerEvent) -> Option<usize> {
        self.covers(event)
            .then(|| self.event_order.get(&event.sequence).copied())
            .flatten()
    }

    fn matches_statement(&self, table: &str, event: TriggerEvent) -> bool {
        self.event == event && self.root_table == table
    }

    fn enter(&self, definition: &uqa_sql::ast::CreateTrigger) -> Result<TransitionRelationScope> {
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
        Ok(TransitionRelationScope::enter(relations))
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
        if matches!(value, Value::Null) {
            continue;
        }
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
    let key = (
        table.to_string(),
        operation_name(event),
        updated_columns.to_vec(),
    );
    if let Some(cached) = TRANSITION_CAPTURE_CACHE.with(|cache| {
        cache
            .borrow()
            .last()
            .and_then(|cache| cache.get(&key).copied())
    }) {
        return Ok(cached);
    }
    let required = compute_transition_capture_required(engine, table, event, updated_columns)?;
    TRANSITION_CAPTURE_CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().last_mut() {
            cache.insert(key, required);
        }
    });
    Ok(required)
}

fn compute_transition_capture_required(
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

struct TransitionEventSchedule {
    generations: BTreeSet<usize>,
    event_generations: BTreeMap<usize, usize>,
    event_order: BTreeMap<usize, usize>,
}

fn trigger_record_field<'a>(record: &'a Value, name: &str) -> Option<&'a Value> {
    let Value::Record(fields) = record else {
        return None;
    };
    fields
        .iter()
        .find_map(|(field, value)| (field == name).then_some(value))
}

fn event_starts_referential_statement(
    engine: &Engine,
    candidate: &AfterRowTriggerEvent,
    target_event: TriggerEvent,
    source_tables: &BTreeSet<String>,
) -> Result<bool> {
    if !matches!(candidate.event, TriggerEvent::Update | TriggerEvent::Delete) {
        return Ok(false);
    }
    for (ref_table, foreign_key) in super::dml::referrers_to_for_actions(engine, &candidate.table)?
    {
        let action = match candidate.event {
            TriggerEvent::Update => foreign_key.on_update,
            TriggerEvent::Delete => foreign_key.on_delete,
            TriggerEvent::Insert | TriggerEvent::Truncate => unreachable!(),
        };
        let statement_event = match (candidate.event, action) {
            (TriggerEvent::Delete, ForeignKeyAction::Cascade) => TriggerEvent::Delete,
            (
                TriggerEvent::Update | TriggerEvent::Delete,
                ForeignKeyAction::Cascade
                | ForeignKeyAction::SetNull
                | ForeignKeyAction::SetDefault,
            ) => TriggerEvent::Update,
            (
                TriggerEvent::Update | TriggerEvent::Delete,
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict,
            ) => continue,
            (TriggerEvent::Insert | TriggerEvent::Truncate, _) => unreachable!(),
        };
        if statement_event != target_event || !source_tables.contains(&ref_table) {
            continue;
        }
        if candidate.event == TriggerEvent::Update
            && foreign_key.ref_columns.iter().all(|column| {
                trigger_record_field(&candidate.old, column)
                    == trigger_record_field(&candidate.new, column)
            })
        {
            continue;
        }
        if candidate.event == TriggerEvent::Delete
            && foreign_key.ref_columns.iter().any(|column| {
                matches!(
                    trigger_record_field(&candidate.old, column),
                    None | Some(Value::Null)
                )
            })
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn transition_event_schedule(
    engine: &Engine,
    event: TriggerEvent,
    source_tables: &BTreeSet<String>,
    events: &[AfterRowTriggerEvent],
    split_cascades: bool,
) -> Result<TransitionEventSchedule> {
    let matching = events
        .iter()
        .filter(|candidate| candidate.event == event && source_tables.contains(&candidate.table))
        .map(|candidate| candidate.sequence)
        .collect::<BTreeSet<_>>();
    let mut roots = Vec::new();
    let mut children = BTreeMap::<usize, Vec<usize>>::new();
    for sequence in &matching {
        let candidate = &events[*sequence];
        if let Some(parent) = candidate
            .cascade_parent
            .filter(|parent| matching.contains(parent))
        {
            children.entry(parent).or_default().push(*sequence);
        } else {
            roots.push(*sequence);
        }
    }
    let mut generations = BTreeSet::from([0]);
    let mut event_generations = BTreeMap::new();
    for root in &roots {
        event_generations.insert(*root, 0);
    }
    let mut queue = VecDeque::from(roots);
    let mut event_order = BTreeMap::new();
    let mut current_generation = 0usize;
    while let Some(sequence) = queue.pop_front() {
        let order = event_order.len();
        event_order.insert(sequence, order);
        let candidate = &events[sequence];
        if split_cascades
            && event_starts_referential_statement(engine, candidate, event, source_tables)?
        {
            generations.insert(current_generation);
        }
        if let Some(descendants) = children.get(&sequence) {
            for descendant in descendants {
                let generation = if split_cascades {
                    current_generation
                } else {
                    0
                };
                event_generations.insert(*descendant, generation);
                generations.insert(generation);
                queue.push_back(*descendant);
            }
        }
        let assigned_generation = event_generations.get(&sequence).copied().unwrap_or(0);
        let closes_transition_set = candidate
            .triggers
            .iter()
            .any(|trigger| !trigger.definition.transition_relations.is_empty());
        if split_cascades && closes_transition_set && assigned_generation == current_generation {
            current_generation += 1;
        }
    }
    Ok(TransitionEventSchedule {
        generations,
        event_generations,
        event_order,
    })
}

pub(super) fn build_transition_tables(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    events: &[AfterRowTriggerEvent],
) -> Result<Vec<TransitionTables>> {
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
        return Ok(Vec::new());
    }
    let source_tables = engine
        .hierarchy_scan_tables(table, true)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let split_cascades = triggers.iter().any(|trigger| trigger.definition.row);
    let schedule =
        transition_event_schedule(engine, event, &source_tables, events, split_cascades)?;
    let mut matching = events
        .iter()
        .filter(|candidate| candidate.event == event && source_tables.contains(&candidate.table))
        .collect::<Vec<_>>();
    matching.sort_by_key(|candidate| {
        schedule
            .event_order
            .get(&candidate.sequence)
            .copied()
            .unwrap_or(candidate.sequence)
    });
    let need_old = triggers
        .iter()
        .any(|trigger| trigger.definition.old_transition_table().is_some());
    let need_new = triggers
        .iter()
        .any(|trigger| trigger.definition.new_transition_table().is_some());
    schedule
        .generations
        .iter()
        .copied()
        .map(|generation| {
            let in_generation = |candidate: &&AfterRowTriggerEvent| {
                schedule.event_generations.get(&candidate.sequence) == Some(&generation)
            };
            let old = need_old
                .then(|| {
                    materialize_transition_rows(
                        engine,
                        table,
                        matching
                            .iter()
                            .filter(|candidate| in_generation(candidate))
                            .map(|candidate| candidate.old.clone()),
                    )
                })
                .transpose()?;
            let new = need_new
                .then(|| {
                    materialize_transition_rows(
                        engine,
                        table,
                        matching
                            .iter()
                            .filter(|candidate| in_generation(candidate))
                            .map(|candidate| candidate.new.clone()),
                    )
                })
                .transpose()?;
            Ok(TransitionTables {
                root_table: table.to_string(),
                event,
                generation,
                event_generations: schedule.event_generations.clone(),
                event_order: schedule.event_order.clone(),
                source_tables: source_tables.clone(),
                old,
                new,
            })
        })
        .collect()
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
    let columns = engine.rule_relation_columns(table)?;
    Ok(columns
        .into_iter()
        .map(|(name, ty)| (name, ty.sql_name()))
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
        TriggerTiming::InsteadOf => "INSTEAD OF",
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
    let _transition_scope = match transition_tables {
        Some(tables) => tables.enter(&invocation.trigger.definition)?,
        None if invocation
            .trigger
            .definition
            .transition_relations
            .is_empty() =>
        {
            TransitionRelationScope::empty()
        }
        None => {
            return Err(SQLError::Internal(format!(
                "trigger `{}` requested unavailable transition tables",
                invocation.trigger.definition.name
            )))
        }
    };
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
            relation_oid: super::catalog::event_relation_oid(engine, invocation.table)?,
            table_name: relation.name,
            table_schema: relation.schema,
            arguments: invocation.trigger.definition.arguments.clone(),
        },
    )
}

fn positional_trigger_record(
    columns: &[(String, uqa_sql::ast::ColumnType)],
    values: Option<&[Value]>,
) -> Result<Value> {
    if values.is_some_and(|values| values.len() != columns.len()) {
        return Err(SQLError::Internal(
            "INSTEAD OF trigger row does not match the view row type".into(),
        ));
    }
    Ok(Value::Record(
        columns
            .iter()
            .enumerate()
            .map(|(position, (name, _))| {
                (
                    name.clone(),
                    values
                        .and_then(|values| values.get(position))
                        .cloned()
                        .unwrap_or(Value::Null),
                )
            })
            .collect(),
    ))
}

fn normalize_instead_of_trigger_record(
    columns: &[(String, uqa_sql::ast::ColumnType)],
    value: Value,
) -> Result<Option<Vec<Value>>> {
    let fields = match value {
        Value::Null => return Ok(None),
        Value::Record(fields) => fields.into_iter().collect::<BTreeMap<_, _>>(),
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "39P01".into(),
                message: "trigger function returned non-composite value".into(),
            })
        }
    };
    if let Some(unknown) = fields
        .keys()
        .find(|name| !columns.iter().any(|(column, _)| column == *name))
    {
        return Err(SQLError::UnknownColumn(unknown.clone()));
    }
    columns
        .iter()
        .map(|(name, ty)| {
            crate::sql::convert_value_to_column_type(
                fields.get(name).cloned().unwrap_or(Value::Null),
                ty,
            )
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

pub(super) fn fire_instead_of_row_triggers(
    engine: &Engine,
    view: &str,
    event: TriggerEvent,
    old_values: Option<&[Value]>,
    new_values: Option<&[Value]>,
    updated_columns: &[String],
) -> Result<Option<Vec<Value>>> {
    let columns = engine.rule_relation_columns(view)?;
    let old = positional_trigger_record(&columns, old_values)?;
    let mut new = positional_trigger_record(&columns, new_values)?;
    let triggers =
        engine.triggers_for(view, TriggerTiming::InsteadOf, event, true, updated_columns)?;
    if triggers.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: format!(
                "cannot {} view \"{}\": no active INSTEAD OF trigger",
                operation_name(event).to_ascii_lowercase(),
                RelationIdentity::from_legacy_name(view)
                    .map_or_else(|_| view.to_string(), |relation| relation.name)
            ),
        });
    }
    for trigger in triggers {
        let returned = invoke_trigger(
            engine,
            TriggerInvocation {
                table: view,
                trigger: &trigger,
                timing: TriggerTiming::InsteadOf,
                event,
                row: true,
                old: old.clone(),
                new: new.clone(),
            },
            None,
        )?;
        let Some(values) = normalize_instead_of_trigger_record(&columns, returned)? else {
            return Ok(None);
        };
        if event != TriggerEvent::Delete {
            new = positional_trigger_record(&columns, Some(&values))?;
        }
    }
    let final_record = if event == TriggerEvent::Delete {
        old
    } else {
        new
    };
    normalize_instead_of_trigger_record(&columns, final_record)
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

fn fire_after_statement_trigger_generation(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: &[TransitionTables],
    generation: usize,
) -> Result<()> {
    let matching = transition_tables
        .iter()
        .filter(|transition| {
            transition.matches_statement(table, event) && transition.generation == generation
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        let has_transition_sets = transition_tables
            .iter()
            .any(|transition| transition.matches_statement(table, event));
        if generation == 0 && !has_transition_sets {
            return fire_after_statement_triggers(engine, table, event, updated_columns, None);
        }
        return Ok(());
    }
    for transition in matching {
        fire_after_statement_triggers(engine, table, event, updated_columns, Some(transition))?;
    }
    Ok(())
}

pub(super) fn after_trigger_generations(transition_tables: &[&TransitionTables]) -> Vec<usize> {
    let mut generations = transition_tables
        .iter()
        .map(|transition| transition.generation)
        .collect::<BTreeSet<_>>();
    if generations.is_empty() {
        generations.insert(0);
    }
    generations.into_iter().collect()
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
            tables.extend(build_transition_tables(
                engine,
                &statement.table,
                statement.event,
                &statement.updated_columns,
                events,
            )?);
        }
        Ok(tables)
    }

    pub(super) fn fire_after(
        &self,
        engine: &Engine,
        transitions: &[TransitionTables],
        root_table: &str,
        root_events: &[TriggerEvent],
        generation: usize,
    ) -> Result<()> {
        let canonical_root = engine
            .try_resolve_table_name(root_table)
            .map_err(|error| SQLError::Internal(format!("resolve trigger root: {error}")))?
            .unwrap_or_else(|| root_table.to_string());
        for statement in &self.after {
            let canonical_statement = engine
                .try_resolve_table_name(&statement.table)
                .map_err(|error| {
                    SQLError::Internal(format!("resolve referential trigger table: {error}"))
                })?
                .unwrap_or_else(|| statement.table.clone());
            if canonical_statement == canonical_root && root_events.contains(&statement.event) {
                continue;
            }
            fire_after_statement_trigger_generation(
                engine,
                &statement.table,
                statement.event,
                &statement.updated_columns,
                transitions,
                generation,
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
    sequence: usize,
    cascade_parent: Option<usize>,
}

pub(super) struct AfterRowTriggerInput<'a> {
    pub(super) table: &'a str,
    pub(super) event: TriggerEvent,
    pub(super) old_doc_id: DocId,
    pub(super) new_doc_id: DocId,
    pub(super) old_document: Option<&'a Document>,
    pub(super) new_document: Option<&'a Document>,
    pub(super) updated_columns: &'a [String],
    pub(super) cascade_parent: Option<usize>,
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
            cascade_parent,
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
            sequence: usize::MAX,
            cascade_parent,
        }))
    }

    pub(super) fn prepare_transition_capture(
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
            cascade_parent,
        } = input;
        if !transition_capture_required(engine, table, event, updated_columns)? {
            return Ok(None);
        }
        let old = match old_document {
            Some(document) => trigger_record(engine, table, old_doc_id, Some(document), false)?,
            None => Value::Null,
        };
        let new = match new_document {
            Some(document) => trigger_record(engine, table, new_doc_id, Some(document), false)?,
            None => Value::Null,
        };
        Ok(Some(Self {
            table: table.to_string(),
            event,
            old,
            new,
            triggers: Vec::new(),
            sequence: usize::MAX,
            cascade_parent,
        }))
    }

    pub(super) fn push(events: &mut Vec<Self>, mut event: Self) -> usize {
        let sequence = events.len();
        event.sequence = sequence;
        events.push(event);
        sequence
    }

    pub(super) fn append(events: &mut Vec<Self>, appended: Vec<Self>) {
        let sequence_offset = events.len();
        debug_assert!(appended
            .iter()
            .enumerate()
            .all(|(sequence, event)| event.sequence == sequence));
        events.reserve(appended.len());
        for mut event in appended {
            event.cascade_parent = event.cascade_parent.map(|parent| sequence_offset + parent);
            Self::push(events, event);
        }
    }
}

pub(super) fn fire_after_row_trigger_events_for_generation(
    engine: &Engine,
    events: &[AfterRowTriggerEvent],
    transition_tables: &[&TransitionTables],
    generation: usize,
) -> Result<()> {
    let mut matching = events
        .iter()
        .filter(|event| {
            transition_tables
                .iter()
                .find_map(|tables| tables.generation_for(event))
                .unwrap_or(0)
                == generation
        })
        .collect::<Vec<_>>();
    matching.sort_by_key(|event| {
        transition_tables
            .iter()
            .find_map(|tables| tables.order_for(event))
            .unwrap_or(event.sequence)
    });
    for event in matching {
        fire_after_row_trigger(engine, event, transition_tables)?;
    }
    Ok(())
}

pub(super) fn fire_after_statement_trigger_generation_for_root(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: &[TransitionTables],
    generation: usize,
) -> Result<()> {
    fire_after_statement_trigger_generation(
        engine,
        table,
        event,
        updated_columns,
        transition_tables,
        generation,
    )
}
