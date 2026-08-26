//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded hash aggregation with mergeable partial-state spill.

use std::collections::BTreeMap;

use super::{
    eval_scalar, fold, partial, AggregateKind, AggregateSpec, Batch, DefaultExpressionEvaluator,
    ExecError, ExecResult, PhysicalOperator, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr,
    SortKey, Value,
};
use crate::PhysicalRow;
use fold::{AggFold, GroupState};

const GROUP_ENTRY_OVERHEAD_BYTES: usize = 192;

pub(super) fn supported(group_keys: &[(String, ScalarExpr)], aggregates: &[AggregateSpec]) -> bool {
    group_keys.is_empty() || aggregates.iter().all(|aggregate| !aggregate.distinct)
}

pub(super) struct AdaptiveBuiltinAggregate<'a> {
    group_keys: &'a [(String, ScalarExpr)],
    aggregates: &'a [AggregateSpec],
    params: &'a [SQLParam],
    state_budget: usize,
    io_budget: usize,
    fold_budget: usize,
    groups: BTreeMap<Vec<Value>, GroupState>,
    retained_bytes: usize,
    partials: Option<crate::spill::SpillBuffer>,
    partial_relation: uqa_sql::ast::InternalRelationId,
    variable_state: bool,
}

impl<'a> AdaptiveBuiltinAggregate<'a> {
    pub(super) fn new(
        group_keys: &'a [(String, ScalarExpr)],
        aggregates: &'a [AggregateSpec],
        params: &'a [SQLParam],
        work_mem_bytes: usize,
    ) -> Self {
        let state_budget = work_mem_bytes
            .saturating_mul(2)
            .checked_div(3)
            .unwrap_or(0)
            .max(1);
        let io_budget = (work_mem_bytes / 3).max(1);
        let fold_budget = (state_budget / aggregates.len().max(1)).max(1);
        let variable_state = aggregates
            .iter()
            .any(|aggregate| matches!(aggregate.kind, AggregateKind::Min | AggregateKind::Max));
        Self {
            group_keys,
            aggregates,
            params,
            state_budget,
            io_budget,
            fold_budget,
            groups: BTreeMap::new(),
            retained_bytes: 0,
            partials: None,
            partial_relation: uqa_sql::ast::InternalRelationId::allocate(),
            variable_state,
        }
    }

    pub(super) fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        for row in batch.rows {
            let view = batch.schema.view(&row);
            let context = ScalarEvalContext::from_row_lookup(&view, self.params);
            let key = self
                .group_keys
                .iter()
                .map(|(_, expression)| eval_scalar(expression, &context))
                .collect::<Result<Vec<_>, _>>()?;
            self.ensure_group(&key)?;
            let state = self.groups.get_mut(&key).ok_or_else(|| {
                ExecError::Other("adaptive aggregate group was not initialized".into())
            })?;
            let previous_bytes = state_bytes(state);
            for (fold, aggregate) in state.folds.iter_mut().zip(self.aggregates) {
                fold::fold_into(fold, aggregate, &view, self.params)?;
            }
            if self.variable_state {
                self.retained_bytes = self
                    .retained_bytes
                    .checked_sub(previous_bytes)
                    .and_then(|bytes| bytes.checked_add(state_bytes(state)))
                    .ok_or_else(|| ExecError::Other("aggregate state size overflow".into()))?;
            }
            if !self.group_keys.is_empty() && self.retained_bytes > self.state_budget {
                self.flush_partial_groups()?;
            }
        }
        Ok(())
    }

    fn ensure_group(&mut self, key: &[Value]) -> ExecResult<()> {
        if self.groups.contains_key(key) {
            return Ok(());
        }
        let state = GroupState {
            folds: self
                .aggregates
                .iter()
                .map(|aggregate| AggFold::new(self.fold_budget, aggregate.distinct))
                .collect(),
            key_values: key.to_vec(),
        };
        // `BTreeMap` owns a second clone of the group key in addition to the
        // copy retained by `GroupState`. Count both dynamic allocations or a
        // workload with long string/list keys can silently exceed work_mem.
        let bytes = group_entry_bytes(key, &state);
        if !self.group_keys.is_empty()
            && !self.groups.is_empty()
            && self
                .retained_bytes
                .checked_add(bytes)
                .is_none_or(|retained| retained > self.state_budget)
        {
            self.flush_partial_groups()?;
        }
        self.retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or_else(|| ExecError::Other("aggregate state size overflow".into()))?;
        self.groups.insert(key.to_vec(), state);
        Ok(())
    }

    fn flush_partial_groups(&mut self) -> ExecResult<()> {
        if self.groups.is_empty() {
            return Ok(());
        }
        let schema = partial::schema(self.partial_relation, self.group_keys.len());
        let partials = self
            .partials
            .get_or_insert_with(|| crate::spill::SpillBuffer::new(self.io_budget));
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        for (_, state) in std::mem::take(&mut self.groups) {
            pending.push(partial::encode_group(state));
            if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
                partials.push(Batch::from_physical_rows(
                    schema.clone(),
                    std::mem::take(&mut pending),
                ))?;
                pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
            }
        }
        if !pending.is_empty() {
            partials.push(Batch::from_physical_rows(schema, pending))?;
        }
        self.retained_bytes = 0;
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        output_schema: RowSchema,
    ) -> ExecResult<crate::spill::SpillBuffer> {
        if self.partials.is_none() {
            return self.finish_memory(output_schema);
        }
        self.flush_partial_groups()?;
        let partials = self
            .partials
            .take()
            .ok_or_else(|| ExecError::Other("partial aggregate spill disappeared".into()))?;
        self.finish_spilled(output_schema, partials)
    }

    fn finish_memory(mut self, output_schema: RowSchema) -> ExecResult<crate::spill::SpillBuffer> {
        if self.groups.is_empty() && self.group_keys.is_empty() {
            self.ensure_group(&[])?;
        }
        let mut output = crate::spill::SpillBuffer::new(self.io_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        for (_, state) in self.groups {
            push_finished(
                &mut output,
                &output_schema,
                &mut pending,
                fold::finalise_builtin_group(state, self.group_keys, self.aggregates)?,
            )?;
        }
        flush_finished(&mut output, &output_schema, &mut pending)?;
        Ok(output)
    }

    fn finish_spilled(
        self,
        output_schema: RowSchema,
        partials: crate::spill::SpillBuffer,
    ) -> ExecResult<crate::spill::SpillBuffer> {
        let group_count = self.group_keys.len();
        let partial_schema = partial::schema(self.partial_relation, group_count);
        let scan: Box<dyn PhysicalOperator> =
            Box::new(crate::spill_scan::SpillScan::new(partial_schema, partials));
        let keys = (0..group_count)
            .map(|index| SortKey {
                expr: ScalarExpr::InternalColumn(self.partial_relation.column(index)),
                descending: false,
                nulls_first: None,
            })
            .collect();
        let evaluator = DefaultExpressionEvaluator::shared(self.params.to_vec());
        let mut sorted =
            crate::external_sort::ExternalSort::new(scan, keys, evaluator, None, self.io_budget);
        sorted.open()?;

        let mut current: Option<GroupState> = None;
        let mut output = crate::spill::SpillBuffer::new(self.io_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        let execution = (|| -> ExecResult<()> {
            while let Some(batch) = sorted.next()? {
                for row in batch.rows {
                    let state = partial::decode_group(row, group_count, self.aggregates)?;
                    if current
                        .as_ref()
                        .is_some_and(|current| current.key_values != state.key_values)
                    {
                        finish_current(
                            &mut current,
                            &mut output,
                            &output_schema,
                            &mut pending,
                            self.group_keys,
                            self.aggregates,
                        )?;
                    }
                    if let Some(current) = current.as_mut() {
                        partial::merge_group(current, state)?;
                    } else {
                        current = Some(state);
                    }
                }
            }
            finish_current(
                &mut current,
                &mut output,
                &output_schema,
                &mut pending,
                self.group_keys,
                self.aggregates,
            )?;
            flush_finished(&mut output, &output_schema, &mut pending)
        })();
        let close = sorted.close();
        crate::physical::with_cleanup(execution, close, "close partial aggregate sort")?;
        Ok(output)
    }
}

fn finish_current(
    state: &mut Option<GroupState>,
    output: &mut crate::spill::SpillBuffer,
    schema: &RowSchema,
    pending: &mut Vec<PhysicalRow>,
    group_keys: &[(String, ScalarExpr)],
    aggregates: &[AggregateSpec],
) -> ExecResult<()> {
    if let Some(state) = state.take() {
        push_finished(
            output,
            schema,
            pending,
            fold::finalise_builtin_group(state, group_keys, aggregates)?,
        )?;
    }
    Ok(())
}

fn push_finished(
    output: &mut crate::spill::SpillBuffer,
    schema: &RowSchema,
    pending: &mut Vec<PhysicalRow>,
    row: PhysicalRow,
) -> ExecResult<()> {
    pending.push(row);
    if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
        output.push(Batch::from_physical_rows(
            schema.clone(),
            std::mem::take(pending),
        ))?;
        *pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

fn flush_finished(
    output: &mut crate::spill::SpillBuffer,
    schema: &RowSchema,
    pending: &mut Vec<PhysicalRow>,
) -> ExecResult<()> {
    if !pending.is_empty() {
        output.push(Batch::from_physical_rows(
            schema.clone(),
            std::mem::take(pending),
        ))?;
    }
    Ok(())
}

fn state_bytes(state: &GroupState) -> usize {
    GROUP_ENTRY_OVERHEAD_BYTES
        .saturating_add(
            state
                .key_values
                .iter()
                .map(value_retained_bytes)
                .sum::<usize>(),
        )
        .saturating_add(
            state
                .folds
                .iter()
                .map(|fold| {
                    std::mem::size_of::<AggFold>()
                        .saturating_add(fold.min.as_ref().map_or(0, value_retained_bytes))
                        .saturating_add(fold.max.as_ref().map_or(0, value_retained_bytes))
                })
                .sum::<usize>(),
        )
}

fn group_entry_bytes(map_key: &[Value], state: &GroupState) -> usize {
    state_bytes(state).saturating_add(map_key.iter().map(value_retained_bytes).sum::<usize>())
}

fn value_retained_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::Str(value) | Value::FixedChar(value) | Value::Json(value) | Value::JsonB(value) => {
            value.capacity()
        }
        Value::Bytes(value) => value.capacity(),
        Value::Array(array) => array
            .retained_header_bytes()
            .saturating_add(
                array
                    .elements()
                    .len()
                    .saturating_mul(std::mem::size_of::<Value>())
                    .saturating_add(array.elements().iter().map(value_retained_bytes).sum()),
            )
            .saturating_add(
                array
                    .lower_bounds()
                    .len()
                    .saturating_mul(std::mem::size_of::<i32>()),
            )
            .saturating_add(
                array
                    .dimensions()
                    .len()
                    .saturating_mul(std::mem::size_of::<usize>()),
            ),
        Value::List(values) | Value::Row(values) => values
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(values.iter().map(value_retained_bytes).sum()),
        Value::Record(fields) => fields.iter().fold(0usize, |bytes, (name, value)| {
            bytes
                .saturating_add(name.capacity())
                .saturating_add(value_retained_bytes(value))
                .saturating_add(2 * std::mem::size_of::<usize>())
        }),
        Value::Map(values) => values.iter().fold(0usize, |bytes, (key, value)| {
            bytes
                .saturating_add(key.capacity())
                .saturating_add(value_retained_bytes(value))
                .saturating_add(3 * std::mem::size_of::<usize>())
        }),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Temporal(_) => 0,
        Value::Decimal(value) => value.retained_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_entry_accounting_includes_both_owned_key_copies() {
        // Keep the string populated: cloning an empty `String` is allowed to
        // discard its spare capacity, so it would not model the two retained
        // key allocations owned by the map and `GroupState`.
        let key = vec![Value::Str("x".repeat(4_096))];
        let state = GroupState {
            folds: vec![AggFold::new(1, false)],
            key_values: key.clone(),
        };

        let one_key_copy = key.iter().map(value_retained_bytes).sum::<usize>();
        assert_eq!(
            group_entry_bytes(&key, &state),
            state_bytes(&state) + one_key_copy
        );
        assert!(group_entry_bytes(&key, &state) >= one_key_copy.saturating_mul(2));
    }
}
