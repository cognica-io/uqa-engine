//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregates evaluated over window frames as `eval_windowaggregates` evaluates them: rows enter the aggregate while the frame head stays put, the aggregate restarts at the frame head when the head moves or an exclusion leaves gaps, and rows that share a frame share its value.

use super::frame::{BoundKind, CurrentRow, FrameCursor, FrameSpec, Membership};
use super::partition::PartitionRows;
use crate::aggregation::{
    aggregate_value_with_args, instantiate_aggregate_accumulators, observe_aggregate,
    AggregateAccumulator, AggregateAccumulatorTemplate,
};
use uqa_core::Value;
use uqa_sql::ast::FrameExclusion;
use uqa_sql::{SQLError, ScalarExpr};

pub(super) struct WindowAggregate {
    name: String,
    args: Vec<ScalarExpr>,
    /// Rows for which this condition is not true do not enter the aggregate.
    filter: Option<ScalarExpr>,
    template: AggregateAccumulatorTemplate,
    budget_bytes: usize,
    accumulator: AggregateAccumulator,
    /// The first row the accumulator holds: the frame head it was built for.
    aggregated_base: i64,
    /// The first row the accumulator has not yet considered.
    aggregated_upto: i64,
    result: Value,
}

impl WindowAggregate {
    /// An aggregate call: its name, arguments and `FILTER` condition.
    pub(super) fn new(
        (name, args, filter): (&str, &[ScalarExpr], Option<&ScalarExpr>),
        template: AggregateAccumulatorTemplate,
        budget_bytes: usize,
    ) -> Self {
        let accumulator = instantiate(&template, budget_bytes);
        Self {
            name: name.to_string(),
            args: args.to_vec(),
            filter: filter.cloned(),
            template,
            budget_bytes,
            accumulator,
            aggregated_base: 0,
            aggregated_upto: 0,
            result: Value::Null,
        }
    }

    pub(super) fn begin_partition(&mut self) {
        self.aggregated_base = 0;
        self.aggregated_upto = 0;
        self.result = Value::Null;
    }

    /// The aggregate over the current row's frame.
    pub(super) fn value(
        &mut self,
        frame: &FrameSpec,
        cursor: &mut FrameCursor,
        current: &mut CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<Value, SQLError> {
        let head = cursor.head(frame, current, rows)?;
        if head < self.aggregated_base {
            return Err(SQLError::Internal(
                "window frame head moved backward".into(),
            ));
        }
        // A frame that starts where the previous one did and ends at the partition end or with the current row's peers is the same frame while the current row lies inside the previous one.
        if self.aggregated_base == head
            && matches!(
                frame.end,
                BoundKind::UnboundedFollowing | BoundKind::CurrentRow
            )
            && frame.exclusion == FrameExclusion::NoOthers
            && self.aggregated_base <= current.position
            && self.aggregated_upto > current.position
        {
            return Ok(self.result.clone());
        }
        if current.position == 0
            || self.aggregated_base != head
            || frame.exclusion != FrameExclusion::NoOthers
            || self.aggregated_upto <= head
        {
            self.accumulator = instantiate(&self.template, self.budget_bytes);
            self.aggregated_upto = head;
        }
        self.aggregated_base = head;
        while self.aggregated_upto < rows.len() {
            match cursor.membership(frame, current, rows, self.aggregated_upto)? {
                Membership::After => break,
                Membership::Outside => {}
                Membership::Inside => {
                    let (name, args, filter, accumulator) =
                        (&self.name, &self.args, &self.filter, &mut self.accumulator);
                    rows.with_context(self.aggregated_upto, |context| {
                        // `advance_windowaggregate`: a row whose filter is not true is skipped.
                        if let Some(filter) = filter {
                            if !uqa_sql::expr::truthy(&crate::eval_scalar(filter, context)?) {
                                return Ok(());
                            }
                        }
                        observe_aggregate(accumulator, name, args, false, &[], context)
                    })?;
                }
            }
            self.aggregated_upto += 1;
        }
        self.result = aggregate_value_with_args(
            &self.name,
            &self.accumulator,
            &self.args,
            rows.enum_labels(),
        )?;
        Ok(self.result.clone())
    }
}

fn instantiate(
    template: &AggregateAccumulatorTemplate,
    budget_bytes: usize,
) -> AggregateAccumulator {
    instantiate_aggregate_accumulators(std::slice::from_ref(template), budget_bytes)
        .pop()
        .expect("one template instantiates one accumulator")
}
