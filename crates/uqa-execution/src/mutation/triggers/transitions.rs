//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The transition tables of a statement's AFTER triggers.

use super::{
    operation_name, BTreeMap, BTreeSet, Result, SQLError, TransitionRelationScope, TriggerContext,
    TriggerEvent, TriggerTiming, Value, TRANSITION_CAPTURE_CACHE,
};

/// The rows that a relation and operation of a statement wrote, as its `OLD TABLE` and `NEW TABLE` transition relations show them.
pub struct TransitionTables {
    pub old: Option<crate::SharedSpill>,
    pub new: Option<crate::SharedSpill>,
}

impl TransitionTables {
    pub fn enter(
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

pub(super) fn materialize_transition_rows(
    context: &TriggerContext<'_>,
    table: &str,
    values: impl IntoIterator<Item = Value>,
) -> Result<crate::SharedSpill> {
    let columns = context
        .relations
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read transition row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let names = columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let schema = crate::RowSchema::with_types(
        names.clone(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let mut spill = crate::SpillBuffer::new(
        crate::query::projection::physical_work_mem_bytes(context.runtime)?.max(1),
    );
    let mut rows = Vec::with_capacity(crate::DEFAULT_BATCH_SIZE);
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
        if rows.len() == crate::DEFAULT_BATCH_SIZE {
            spill
                .push(crate::Batch::new(schema.clone(), std::mem::take(&mut rows)))
                .map_err(crate::physical::physical_exec_error)?;
            rows.reserve(crate::DEFAULT_BATCH_SIZE);
        }
    }
    if !rows.is_empty() {
        spill
            .push(crate::Batch::new(schema.clone(), rows))
            .map_err(crate::physical::physical_exec_error)?;
    }
    spill
        .into_shared(schema)
        .map_err(crate::physical::physical_exec_error)
}

pub fn transition_capture_required(
    context: &TriggerContext<'_>,
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
    let required = compute_transition_capture_required(context, table, event, updated_columns)?;
    TRANSITION_CAPTURE_CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().last_mut() {
            cache.insert(key, required);
        }
    });
    Ok(required)
}

fn compute_transition_capture_required(
    context: &TriggerContext<'_>,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<bool> {
    let canonical = context
        .relations
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
            if context
                .catalog
                .triggers_for(&source, TriggerTiming::After, event, row, updated_columns)?
                .iter()
                .any(|trigger| !trigger.definition.transition_relations.is_empty())
            {
                return Ok(true);
            }
        }
        let hierarchy = context
            .relations
            .try_table_hierarchy(&source)
            .map_err(|error| {
                SQLError::Internal(format!("read transition source hierarchy: {error}"))
            })?;
        pending.extend(hierarchy.parents);
    }
    Ok(false)
}
