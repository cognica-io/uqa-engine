//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finalization for in-memory and spilled adaptive aggregate state.

use crate::{ExternalSort, PhysicalOperator, PhysicalRow, RowSchema, SortKey, SpillScan};

use super::{
    AdaptiveAggregateSet, AggregateAccumulator, QueryBlockPlan, QueryExpressionContext, SQLError,
    SQLParam, ScalarExpr, SpillBuffer, Value,
};

impl AdaptiveAggregateSet {
    pub(in crate::aggregation) fn finish(
        mut self,
        context: &dyn QueryExpressionContext,
        output_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<SpillBuffer, SQLError> {
        if self.partials.is_none() {
            return self.finish_memory(context, output_schema, params);
        }
        self.flush_partial_groups()?;
        let partials = self
            .partials
            .take()
            .ok_or_else(|| SQLError::Internal("partial aggregate spill disappeared".into()))?;
        self.finish_spilled(context, output_schema, params, partials)
    }

    fn finish_memory(
        mut self,
        context: &dyn QueryExpressionContext,
        output_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<SpillBuffer, SQLError> {
        if self.groups.is_empty() && self.statement.group_by.is_empty() {
            self.ensure_active_group(&[])?;
        }
        let mut output = SpillBuffer::new(self.spill_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        for entry in self.groups {
            if let Some(row) = super::super::output::finish_group(
                context,
                &self.statement,
                &self.output_plan,
                entry.state.accumulators,
                &entry.key,
                output_schema.columns(),
                params,
            )? {
                super::super::output::push_output_row(
                    &mut output,
                    output_schema,
                    &mut pending,
                    row,
                )?;
            }
        }
        super::super::output::flush_output_rows(&mut output, output_schema, &mut pending)?;
        Ok(output)
    }

    fn finish_spilled(
        self,
        context: &dyn QueryExpressionContext,
        output_schema: &RowSchema,
        params: &[SQLParam],

        partials: SpillBuffer,
    ) -> Result<SpillBuffer, SQLError> {
        let group_count = self.statement.group_by.len();
        let partial_schema =
            super::super::partial_state::partial_schema(self.partial_relation, group_count);
        let scan: Box<dyn PhysicalOperator + '_> =
            Box::new(SpillScan::new(partial_schema, partials));
        let keys = (0..group_count)
            .map(|index| SortKey {
                expr: ScalarExpr::InternalColumn(self.partial_relation.column(index)),
                descending: false,
                nulls_first: None,
            })
            .collect();
        let evaluator = context.expression_evaluator(params);
        let mut sorted = ExternalSort::new(scan, keys, evaluator, None, self.spill_budget);
        sorted
            .open()
            .map_err(super::super::sort_fallback::exec_to_sql_error)?;
        let mut current_key: Option<Vec<Value>> = None;
        let mut current_accumulators = Vec::new();
        let mut output = SpillBuffer::new(self.spill_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        let execution = (|| -> Result<(), SQLError> {
            while let Some(batch) = sorted
                .next()
                .map_err(super::super::sort_fallback::exec_to_sql_error)?
            {
                for row in batch.rows {
                    let (key, accumulators) = super::super::partial_state::decode_partial_group(
                        row,
                        &self.aggregate_targets,
                        self.accumulator_budget,
                        group_count,
                    )?;
                    if current_key.as_ref().is_some_and(|current| current != &key) {
                        finish_merged_group(
                            context,
                            &self.statement,
                            &self.output_plan,
                            params,
                            output_schema,
                            &mut output,
                            &mut pending,
                            current_key.take().unwrap_or_default(),
                            std::mem::take(&mut current_accumulators),
                        )?;
                    }
                    if current_key.is_none() {
                        current_key = Some(key);
                        current_accumulators = accumulators;
                    } else {
                        for (target, source) in current_accumulators.iter_mut().zip(accumulators) {
                            super::super::partial_state::merge_accumulators(target, source)?;
                        }
                    }
                }
            }
            if let Some(key) = current_key.take() {
                finish_merged_group(
                    context,
                    &self.statement,
                    &self.output_plan,
                    params,
                    output_schema,
                    &mut output,
                    &mut pending,
                    key,
                    current_accumulators,
                )?;
            }
            super::super::output::flush_output_rows(&mut output, output_schema, &mut pending)
        })();
        let close = sorted
            .close()
            .map_err(super::super::sort_fallback::exec_to_sql_error);
        super::super::sort_fallback::combine_execution_and_close(
            execution,
            close,
            "partial aggregate sort",
        )?;
        Ok(output)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps execution context inputs aligned"
)]
fn finish_merged_group(
    context: &dyn QueryExpressionContext,
    statement: &QueryBlockPlan,
    output_plan: &super::super::output::AggregateOutputPlan,
    params: &[SQLParam],

    output_schema: &RowSchema,
    output: &mut SpillBuffer,
    pending: &mut Vec<PhysicalRow>,
    key: Vec<Value>,
    accumulators: Vec<AggregateAccumulator>,
) -> Result<(), SQLError> {
    if let Some(row) = super::super::output::finish_group(
        context,
        statement,
        output_plan,
        accumulators,
        &key,
        output_schema.columns(),
        params,
    )? {
        super::super::output::push_output_row(output, output_schema, pending, row)?;
    }
    Ok(())
}
