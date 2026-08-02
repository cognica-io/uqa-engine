//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adaptive streaming/hash aggregation with compact partial-state spill.

mod finish;
mod projected;

use super::{
    aggregate_exprs, eval_scalar, new_aggregate_accumulators_with_budget, AggregateAccumulator,
    BTreeMap, CteScope, Engine, PlanSubqueryArena, QueryBlockPlan, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, ScopedEngineHook, SpillBuffer, Value,
};
use uqa_execution::Batch;

const GROUP_ENTRY_OVERHEAD_BYTES: usize = 256;
const PROJECTED_GROUP_LINEAR_LOOKUP_LIMIT: usize = 32;

struct GroupState {
    accumulators: Vec<AggregateAccumulator>,
    retained_bytes: usize,
    projected_fingerprint: Option<u64>,
}

pub(super) struct AdaptiveAggregateSet {
    statement: QueryBlockPlan,
    aggregate_targets: Vec<ScalarExpr>,
    relaxed: bool,
    state_budget: usize,
    spill_budget: usize,
    accumulator_budget: usize,
    groups: BTreeMap<Vec<Value>, GroupState>,
    retained_bytes: usize,
    partials: Option<SpillBuffer>,
    variable_state: bool,
    projected_group_columns: Option<Vec<super::projected::ProjectedGroupColumn>>,
    projected_aggregate_plans: super::projected_input::ProjectedAggregatePlans,
}

impl AdaptiveAggregateSet {
    pub(super) fn new(
        engine: &Engine,
        statement: QueryBlockPlan,
        relaxed: bool,
        work_mem_bytes: usize,
        input_schema: &[String],
    ) -> Self {
        let aggregate_targets = aggregate_exprs(engine, &statement.projections)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let target_count = aggregate_targets.len().max(1);
        let state_budget = work_mem_bytes
            .saturating_mul(2)
            .checked_div(3)
            .unwrap_or(0)
            .max(1);
        let spill_budget = (work_mem_bytes / 3).max(1);
        let accumulator_budget = (state_budget / target_count / 2).max(1);
        let variable_state = aggregate_targets
            .iter()
            .any(|expression| contains_named_function(expression, &["min", "max"]));
        let projected_group_columns =
            super::projected::ProjectedGroupColumn::compile(&statement.group_by, input_schema);
        let projected_aggregate_plans = super::projected_input::ProjectedAggregatePlans::compile(
            engine,
            &aggregate_targets,
            input_schema,
        );
        Self {
            statement,
            aggregate_targets,
            relaxed,
            state_budget,
            spill_budget,
            accumulator_budget,
            groups: BTreeMap::new(),
            retained_bytes: 0,
            partials: None,
            variable_state,
            projected_group_columns,
            projected_aggregate_plans,
        }
    }

    pub(super) fn statement_subqueries_are_empty(&self) -> bool {
        self.statement.subqueries.is_empty()
    }

    pub(super) fn consume(
        &mut self,
        engine: &Engine,
        batch: &Batch,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<(), SQLError> {
        let hook = ScopedEngineHook::new(engine, ctes);
        let subqueries = self.statement.subqueries.clone();
        let subquery_arena = PlanSubqueryArena::new(&subqueries, Some(&hook));
        for row in &batch.rows {
            let context = ScalarEvalContext::new(Some(row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subquery_arena);
            self.consume_context(engine, &context)?;
        }
        Ok(())
    }

    fn consume_context(
        &mut self,
        engine: &Engine,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let key = self
            .statement
            .group_by
            .iter()
            .map(|expression| eval_scalar(expression, context))
            .collect::<Result<Vec<_>, _>>()?;
        self.consume_key_context(engine, &key, context)
    }

    fn consume_key_context(
        &mut self,
        engine: &Engine,
        key: &[Value],
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        self.ensure_group(engine, key)?;
        let state = self.groups.get_mut(key).ok_or_else(|| {
            SQLError::Internal("adaptive aggregate group was not initialized".into())
        })?;
        let previous_bytes = state.retained_bytes;
        super::sort_fallback::observe_targets(
            &mut state.accumulators,
            &self.aggregate_targets,
            context,
        )?;
        if self.variable_state {
            state.retained_bytes = estimate_group_bytes(key, &state.accumulators);
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(previous_bytes)
                .and_then(|bytes| bytes.checked_add(state.retained_bytes))
                .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        }
        if !self.statement.group_by.is_empty() && self.retained_bytes > self.state_budget {
            self.flush_partial_groups()?;
        }
        Ok(())
    }

    fn ensure_group(&mut self, engine: &Engine, key: &[Value]) -> Result<(), SQLError> {
        if self.groups.contains_key(key) {
            return Ok(());
        }
        let accumulators = new_aggregate_accumulators_with_budget(
            engine,
            &self.aggregate_targets,
            self.accumulator_budget,
        )?;
        let group_bytes = estimate_group_bytes(key, &accumulators);
        if !self.statement.group_by.is_empty()
            && !self.groups.is_empty()
            && self
                .retained_bytes
                .checked_add(group_bytes)
                .is_none_or(|bytes| bytes > self.state_budget)
        {
            self.flush_partial_groups()?;
        }
        self.retained_bytes = self
            .retained_bytes
            .checked_add(group_bytes)
            .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        self.groups.insert(
            key.to_vec(),
            GroupState {
                accumulators,
                retained_bytes: group_bytes,
                projected_fingerprint: None,
            },
        );
        Ok(())
    }

    fn flush_partial_groups(&mut self) -> Result<(), SQLError> {
        if self.groups.is_empty() {
            return Ok(());
        }
        let schema = super::partial_state::partial_schema(self.statement.group_by.len());
        let partials = self
            .partials
            .get_or_insert_with(|| SpillBuffer::new(self.spill_budget));
        let mut pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
        for (key, state) in std::mem::take(&mut self.groups) {
            pending.push(super::partial_state::encode_partial_group(
                key,
                state.accumulators,
            ));
            if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
                partials
                    .push(Batch::new(schema.clone(), std::mem::take(&mut pending)))
                    .map_err(super::sort_fallback::exec_to_sql_error)?;
                pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
            }
        }
        if !pending.is_empty() {
            partials
                .push(Batch::new(schema, pending))
                .map_err(super::sort_fallback::exec_to_sql_error)?;
        }
        self.retained_bytes = 0;
        Ok(())
    }
}

pub(super) fn supports_adaptive_grouping(engine: &Engine, statement: &QueryBlockPlan) -> bool {
    if statement.group_by.is_empty() {
        return true;
    }
    aggregate_exprs(engine, &statement.projections)
        .into_iter()
        .all(|target| match target {
            ScalarExpr::Func {
                name,
                distinct,
                order_by,
                ..
            } => {
                !engine.has_registered_aggregate_function(name)
                    && !distinct
                    && order_by.is_empty()
                    && is_mergeable_builtin(name)
            }
            _ => false,
        })
}

fn is_mergeable_builtin(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
    )
}

fn contains_named_function(expression: &ScalarExpr, names: &[&str]) -> bool {
    match expression {
        ScalarExpr::Func { name, args, .. } => {
            names
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
                || args
                    .iter()
                    .any(|argument| contains_named_function(argument, names))
        }
        _ => false,
    }
}

fn estimate_group_bytes(key: &[Value], accumulators: &[AggregateAccumulator]) -> usize {
    GROUP_ENTRY_OVERHEAD_BYTES
        .saturating_add(key.iter().map(value_retained_bytes).sum::<usize>())
        .saturating_add(
            accumulators
                .iter()
                .map(|accumulator| {
                    std::mem::size_of::<AggregateAccumulator>()
                        .saturating_add(accumulator.min.as_ref().map_or(0, value_retained_bytes))
                        .saturating_add(accumulator.max.as_ref().map_or(0, value_retained_bytes))
                })
                .sum::<usize>(),
        )
}

fn value_retained_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::Str(value) => value.capacity(),
        Value::Bytes(value) => value.capacity(),
        Value::List(values) => values
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(values.iter().map(value_retained_bytes).sum()),
        Value::Map(values) => values.iter().fold(0usize, |bytes, (key, value)| {
            bytes
                .saturating_add(key.capacity())
                .saturating_add(value_retained_bytes(value))
                .saturating_add(3 * std::mem::size_of::<usize>())
        }),
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Decimal(_) => 0,
    })
}
