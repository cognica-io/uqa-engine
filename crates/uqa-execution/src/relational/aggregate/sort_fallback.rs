//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sort aggregation fallback for grouped DISTINCT state.

use super::{
    eval_scalar, fold, AggregateSpec, Batch, DefaultExpressionEvaluator, ExecError, ExecResult,
    PhysicalOperator, ResultRow, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr, SortKey,
    Value,
};
use fold::{AggFold, GroupState};

pub(super) fn execute(
    child: &mut dyn PhysicalOperator,
    group_keys: &[(String, ScalarExpr)],
    aggregates: &[AggregateSpec],
    params: &[SQLParam],
    output_schema: RowSchema,
    work_mem_bytes: usize,
) -> ExecResult<crate::spill::SpillBuffer> {
    let phase_budget = (work_mem_bytes / 3).max(1);
    let mut input = crate::spill::SpillBuffer::new(phase_budget);
    while let Some(batch) = child.next()? {
        input.push(batch)?;
    }
    let scan: Box<dyn PhysicalOperator> = Box::new(crate::spill_scan::SpillScan::new(
        child.schema().to_vec(),
        input,
    ));
    let keys = group_keys
        .iter()
        .map(|(_, expression)| SortKey {
            expr: expression.clone(),
            descending: false,
            nulls_first: None,
        })
        .collect();
    let evaluator = DefaultExpressionEvaluator::shared(params.to_vec());
    let mut sorted =
        crate::external_sort::ExternalSort::new(scan, keys, evaluator, None, phase_budget);
    sorted.open()?;

    let fold_budget = (phase_budget / aggregates.len().max(1)).max(1);
    let mut current_key: Option<Vec<Value>> = None;
    let mut current_state: Option<GroupState> = None;
    let mut output = crate::spill::SpillBuffer::new(phase_budget);
    let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
    let execution = (|| -> ExecResult<()> {
        while let Some(batch) = sorted.next()? {
            for row in batch.rows {
                let context = ScalarEvalContext::new(Some(&row), params);
                let key_values = group_keys
                    .iter()
                    .map(|(_, expression)| eval_scalar(expression, &context))
                    .collect::<Result<Vec<_>, _>>()?;
                if current_key
                    .as_ref()
                    .is_some_and(|current| current != &key_values)
                {
                    finish_group(
                        &mut current_state,
                        group_keys,
                        aggregates,
                        &output_schema,
                        &mut output,
                        &mut pending,
                    )?;
                    current_key = None;
                }
                if current_key.is_none() {
                    current_key = Some(key_values.clone());
                    current_state = Some(GroupState {
                        folds: aggregates
                            .iter()
                            .map(|aggregate| AggFold::new(fold_budget, aggregate.distinct))
                            .collect(),
                        key_values,
                    });
                }
                let state = current_state.as_mut().ok_or_else(|| {
                    ExecError::Other("aggregate group state was not initialized".into())
                })?;
                for (fold, aggregate) in state.folds.iter_mut().zip(aggregates) {
                    fold::fold_into(fold, aggregate, &row, params)?;
                }
            }
        }
        Ok(())
    })();
    let close = sorted.close();
    crate::physical::with_cleanup(execution, close, "close aggregate sort")?;

    if current_state.is_some() {
        finish_group(
            &mut current_state,
            group_keys,
            aggregates,
            &output_schema,
            &mut output,
            &mut pending,
        )?;
    } else if group_keys.is_empty() {
        let state = GroupState {
            folds: aggregates
                .iter()
                .map(|aggregate| AggFold::new(fold_budget, aggregate.distinct))
                .collect(),
            key_values: Vec::new(),
        };
        pending.push(fold::finalise_builtin_group(state, group_keys, aggregates)?);
    }
    flush_pending(&mut output, &output_schema, &mut pending)?;
    Ok(output)
}

fn finish_group(
    state: &mut Option<GroupState>,
    group_keys: &[(String, ScalarExpr)],
    aggregates: &[AggregateSpec],
    schema: &RowSchema,
    output: &mut crate::spill::SpillBuffer,
    pending: &mut Vec<ResultRow>,
) -> ExecResult<()> {
    let state = state
        .take()
        .ok_or_else(|| ExecError::Other("active aggregate group has no state".into()))?;
    pending.push(fold::finalise_builtin_group(state, group_keys, aggregates)?);
    if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
        output.push(Batch::new(schema.clone(), std::mem::take(pending)))?;
        *pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

fn flush_pending(
    output: &mut crate::spill::SpillBuffer,
    schema: &RowSchema,
    pending: &mut Vec<ResultRow>,
) -> ExecResult<()> {
    if !pending.is_empty() {
        output.push(Batch::new(schema.clone(), std::mem::take(pending)))?;
    }
    Ok(())
}
