//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    operation_name, AfterRowTriggerEvent, BTreeMap, BTreeSet, Engine, ForeignKeyAction, Result,
    SQLError, TransitionRelationScope, TriggerEvent, TriggerTiming, Value, VecDeque,
    TRANSITION_CAPTURE_CACHE,
};

pub(in crate::sql) struct TransitionTables {
    pub(super) root_table: String,
    pub(super) event: TriggerEvent,
    pub(super) generation: usize,
    pub(super) event_generations: BTreeMap<usize, usize>,
    pub(super) event_order: BTreeMap<usize, usize>,
    pub(super) source_tables: BTreeSet<String>,
    pub(super) old: Option<uqa_execution::SharedSpill>,
    pub(super) new: Option<uqa_execution::SharedSpill>,
}

impl TransitionTables {
    pub(super) fn covers(&self, event: &AfterRowTriggerEvent) -> bool {
        self.event == event.event && self.source_tables.contains(&event.table)
    }

    pub(super) fn applies_to(&self, event: &AfterRowTriggerEvent) -> bool {
        self.covers(event) && self.event_generations.get(&event.sequence) == Some(&self.generation)
    }

    pub(super) fn generation_for(&self, event: &AfterRowTriggerEvent) -> Option<usize> {
        self.covers(event)
            .then(|| self.event_generations.get(&event.sequence).copied())
            .flatten()
    }

    pub(super) fn order_for(&self, event: &AfterRowTriggerEvent) -> Option<usize> {
        self.covers(event)
            .then(|| self.event_order.get(&event.sequence).copied())
            .flatten()
    }

    pub(super) fn matches_statement(&self, table: &str, event: TriggerEvent) -> bool {
        self.event == event && self.root_table == table
    }

    pub(super) fn enter(
        &self,
        definition: &uqa_sql::ast::CreateTrigger,
    ) -> Result<TransitionRelationScope> {
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
        crate::sql::select::physical_work_mem_bytes(engine.query_runtime_view())?.max(1),
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

pub(in crate::sql) fn transition_capture_required(
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
    for (ref_table, foreign_key) in
        super::super::dml::referrers_to_for_actions(engine, &candidate.table)?
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

pub(in crate::sql) fn build_transition_tables(
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
