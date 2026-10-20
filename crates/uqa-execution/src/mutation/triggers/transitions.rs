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
        names,
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    spool_transition_rows(
        schema,
        values,
        crate::query::projection::physical_work_mem_bytes(context.runtime)?.max(1),
    )
}

fn spool_transition_rows(
    schema: crate::RowSchema,
    values: impl IntoIterator<Item = Value>,
    work_mem: usize,
) -> Result<crate::SharedSpill> {
    let mut spill = crate::SpillBuffer::new(work_mem);
    let mut writer = crate::spill::SpillBatchWriter::new(schema.clone())
        .map_err(crate::physical::physical_exec_error)?;
    for value in values {
        if matches!(value, Value::Null) {
            continue;
        }
        let Value::Record(fields) = value else {
            return Err(SQLError::Internal(
                "transition relation row is not a record".into(),
            ));
        };
        let mut fields = fields.into_iter().collect::<BTreeMap<_, _>>();
        let row = crate::PhysicalRow::from_values(
            schema
                .columns()
                .iter()
                .map(|name| fields.remove(name).unwrap_or(Value::Null))
                .collect(),
        );
        writer
            .push(&mut spill, row)
            .map_err(crate::physical::physical_exec_error)?;
    }
    writer
        .finish(&mut spill)
        .map_err(crate::physical::physical_exec_error)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::ast::ColumnType;

    #[test]
    fn transition_rows_keep_nullary_cardinality_and_reject_nonrecords() {
        let values = std::iter::repeat_with(|| Value::Record(Vec::new().into()))
            .take(4097)
            .chain(std::iter::once(Value::Null));
        let output = spool_transition_rows(crate::RowSchema::default(), values, 64).unwrap();
        let mut count = 0;
        for row in output.read_rows().unwrap() {
            assert!(row.unwrap().row.value(0).is_none());
            count += 1;
        }
        assert_eq!(count, 4097);
        let repeated = crate::RowSchema::with_types(
            vec!["value".into(), "value".into()],
            vec![Some(ColumnType::Integer); 2],
        );
        let output = spool_transition_rows(
            repeated,
            [Value::Record(vec![("value".into(), Value::Int(7))].into())],
            64,
        )
        .unwrap();
        let row = output.read_rows().unwrap().next().unwrap().unwrap();
        assert_eq!(row.row.value(0), Some(&Value::Int(7)));
        assert_eq!(row.row.value(1), Some(&Value::Null));
        let result = spool_transition_rows(
            crate::RowSchema::default(),
            [Value::Record(Vec::new().into()), Value::Int(1)],
            64,
        );
        assert!(matches!(
            result,
            Err(SQLError::Internal(message)) if message == "transition relation row is not a record"
        ));
    }

    #[test]
    fn transition_row_batches_bound_wide_payloads_and_preserve_named_projection() {
        let schema = crate::RowSchema::with_types(
            vec!["id".into(), "body".into(), "missing".into()],
            vec![
                Some(ColumnType::Integer),
                Some(ColumnType::Text),
                Some(ColumnType::Text),
            ],
        );
        let allocated = allocation_counter::measure(|| {
            let values = (0..512).flat_map(|id| {
                [
                    Value::Null,
                    Value::Record(
                        vec![
                            (
                                "body".into(),
                                Value::Str(format!("{id:04}{}", "x".repeat(32764))),
                            ),
                            ("ignored".into(), Value::Int(-1)),
                            ("id".into(), Value::Int(id)),
                        ]
                        .into(),
                    ),
                ]
            });
            let output = spool_transition_rows(schema.clone(), values, 16 * 1024).unwrap();
            for _ in 0..2 {
                let mut count = 0;
                for row in output.read_rows().unwrap() {
                    let row = row.unwrap();
                    assert_eq!(row.row.value(0), Some(&Value::Int(count)));
                    let Some(Value::Str(body)) = row.row.value(1) else {
                        panic!("text body")
                    };
                    assert_eq!(body.len(), 32768);
                    assert_eq!(&body[..4], format!("{count:04}"));
                    assert!(body.as_bytes()[4..].iter().all(|byte| *byte == b'x'));
                    assert_eq!(row.row.value(2), Some(&Value::Null));
                    count += 1;
                }
                assert_eq!(count, 512);
            }
        });
        // A 16MiB transition input is generated and replayed under the original 16KiB spool allowance.
        assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    }
}
