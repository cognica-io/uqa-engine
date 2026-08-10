//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adaptive streaming/hash aggregation with compact partial-state spill.

mod finish;
mod projected;

use std::collections::HashMap;

use smallvec::SmallVec;

use super::{
    aggregate_accumulator_templates, aggregate_targets, eval_scalar,
    instantiate_aggregate_accumulators, AggregateAccumulator, AggregateAccumulatorTemplate,
    CteScope, Engine, PlanSubqueryArena, QueryBlockPlan, SQLError, SQLParam, ScalarEvalContext,
    ScalarExpr, ScopedEngineHook, SpillBuffer, Value,
};
use uqa_execution::{hash_canonical_row, Batch};

const GROUP_ENTRY_OVERHEAD_BYTES: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OverflowStrategy {
    SpillMergeablePartials,
    AbandonForReplay,
}

struct GroupState {
    accumulators: Vec<AggregateAccumulator>,
    retained_bytes: usize,
}

struct GroupEntry {
    key: Vec<Value>,
    state: GroupState,
}

type GroupIndexBucket = SmallVec<[usize; 1]>;
type GroupIndex = HashMap<u64, GroupIndexBucket, ahash::RandomState>;

pub(super) struct AdaptiveAggregateSet {
    statement: QueryBlockPlan,
    aggregate_targets: Vec<ScalarExpr>,
    accumulator_templates: Vec<AggregateAccumulatorTemplate>,
    output_plan: super::output::AggregateOutputPlan,
    state_budget: usize,
    spill_budget: usize,
    accumulator_budget: usize,
    groups: Vec<GroupEntry>,
    group_index: GroupIndex,
    retained_bytes: usize,
    partials: Option<SpillBuffer>,
    variable_state: bool,
    projected_group_columns: Option<Vec<super::projected::ProjectedGroupColumn>>,
    projected_aggregate_plans: super::projected_input::ProjectedAggregatePlans,
    overflow_strategy: OverflowStrategy,
    abandoned: bool,
}

impl AdaptiveAggregateSet {
    pub(super) fn new(
        engine: &Engine,
        statement: QueryBlockPlan,
        relaxed: bool,
        work_mem_bytes: usize,
        input_schema: &[String],
    ) -> Result<Self, SQLError> {
        let aggregate_targets = aggregate_targets(engine, &statement)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let accumulator_templates = aggregate_accumulator_templates(engine, &aggregate_targets);
        let output_plan = super::output::AggregateOutputPlan::compile(
            engine,
            &statement,
            &aggregate_targets,
            relaxed,
        )?;
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
        Ok(Self {
            statement,
            aggregate_targets,
            accumulator_templates,
            output_plan,
            state_budget,
            spill_budget,
            accumulator_budget,
            groups: Vec::new(),
            group_index: HashMap::with_hasher(ahash::RandomState::new()),
            retained_bytes: 0,
            partials: None,
            variable_state,
            projected_group_columns,
            projected_aggregate_plans,
            overflow_strategy: OverflowStrategy::SpillMergeablePartials,
            abandoned: false,
        })
    }

    /// Try hash aggregation for a non-mergeable DISTINCT state while the raw
    /// input is retained by the executor. If the hash state reaches its own
    /// budget, the executor drops it and replays the bounded input through the
    /// canonical sort aggregate.
    pub(super) fn new_optimistic(
        engine: &Engine,
        statement: QueryBlockPlan,
        relaxed: bool,
        state_budget: usize,
        output_budget: usize,
        input_schema: &[String],
    ) -> Result<Self, SQLError> {
        let mut set = Self::new(engine, statement, relaxed, state_budget, input_schema)?;
        set.state_budget = state_budget.max(1);
        set.spill_budget = output_budget.max(1);
        set.accumulator_budget = (set.state_budget / set.aggregate_targets.len().max(1) / 2).max(1);
        set.variable_state = true;
        set.overflow_strategy = OverflowStrategy::AbandonForReplay;
        Ok(set)
    }

    pub(super) fn is_abandoned(&self) -> bool {
        self.abandoned
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
        if self.abandoned {
            return Ok(());
        }
        let hook = ScopedEngineHook::new(engine, ctes);
        let subqueries = self.statement.subqueries.clone();
        let subquery_arena = PlanSubqueryArena::new(&subqueries, Some(&hook));
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let context = ScalarEvalContext::from_row_lookup(&view, params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subquery_arena);
            if !self.consume_projected_group(&view, &context)? {
                self.consume_context(&context)?;
            }
            if self.abandoned {
                break;
            }
        }
        Ok(())
    }

    fn consume_context(&mut self, context: &ScalarEvalContext<'_>) -> Result<(), SQLError> {
        let key = self
            .statement
            .group_by
            .iter()
            .map(|expression| eval_scalar(expression, context))
            .collect::<Result<Vec<_>, _>>()?;
        self.consume_key_context(&key, context)
    }

    fn consume_key_context(
        &mut self,
        key: &[Value],
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let hash = self.group_hash(key)?;
        if self.observe_key_context(hash, key, context)? {
            return self.handle_state_overflow();
        }
        if !self.insert_group(key, hash)? {
            return Ok(());
        }
        if !self.observe_key_context(hash, key, context)? {
            return Err(SQLError::Internal(
                "adaptive aggregate group was not initialized".into(),
            ));
        }
        self.handle_state_overflow()
    }

    fn observe_key_context(
        &mut self,
        hash: u64,
        key: &[Value],
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(index) = matching_group_index(&self.group_index, &self.groups, hash, key) else {
            return Ok(false);
        };
        let entry = &mut self.groups[index];
        let state = &mut entry.state;
        let previous_bytes = state.retained_bytes;
        super::sort_fallback::observe_targets(
            &mut state.accumulators,
            &self.aggregate_targets,
            context,
        )?;
        if self.variable_state {
            state.retained_bytes = estimate_group_bytes(&entry.key, &state.accumulators);
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(previous_bytes)
                .and_then(|bytes| bytes.checked_add(state.retained_bytes))
                .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        }
        Ok(true)
    }

    fn group_hash(&self, key: &[Value]) -> Result<u64, SQLError> {
        hash_canonical_row(self.group_index.hasher(), key.iter().map(Some))
            .map_err(super::sort_fallback::exec_to_sql_error)
    }

    fn insert_group(&mut self, key: &[Value], hash: u64) -> Result<bool, SQLError> {
        if self.abandoned {
            return Ok(false);
        }
        let accumulators = instantiate_aggregate_accumulators(
            &self.accumulator_templates,
            self.accumulator_budget,
        );
        let group_bytes = estimate_group_bytes(key, &accumulators);
        if !self.statement.group_by.is_empty()
            && self
                .retained_bytes
                .checked_add(group_bytes)
                .is_none_or(|bytes| bytes > self.state_budget)
        {
            match self.overflow_strategy {
                OverflowStrategy::SpillMergeablePartials if !self.groups.is_empty() => {
                    self.flush_partial_groups()?;
                }
                OverflowStrategy::SpillMergeablePartials => {}
                OverflowStrategy::AbandonForReplay => {
                    self.abandon();
                    return Ok(false);
                }
            }
        }
        self.retained_bytes = self
            .retained_bytes
            .checked_add(group_bytes)
            .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        let index = self.groups.len();
        self.groups.push(GroupEntry {
            key: key.to_vec(),
            state: GroupState {
                accumulators,
                retained_bytes: group_bytes,
            },
        });
        self.group_index.entry(hash).or_default().push(index);
        Ok(true)
    }

    fn ensure_active_group(&mut self, key: &[Value]) -> Result<u64, SQLError> {
        let hash = self.group_hash(key)?;
        let exists = matching_group_index(&self.group_index, &self.groups, hash, key).is_some();
        if !exists && !self.insert_group(key, hash)? {
            Err(SQLError::Internal(
                "abandoned aggregate state cannot accept projected rows".into(),
            ))
        } else {
            Ok(hash)
        }
    }

    fn handle_state_overflow(&mut self) -> Result<(), SQLError> {
        if self.statement.group_by.is_empty() || self.retained_bytes <= self.state_budget {
            return Ok(());
        }
        match self.overflow_strategy {
            OverflowStrategy::SpillMergeablePartials => self.flush_partial_groups(),
            OverflowStrategy::AbandonForReplay => {
                self.abandon();
                Ok(())
            }
        }
    }

    fn abandon(&mut self) {
        self.groups.clear();
        self.group_index.clear();
        self.partials = None;
        self.retained_bytes = 0;
        self.abandoned = true;
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
        let mut groups = std::mem::take(&mut self.groups);
        self.group_index.clear();
        for entry in groups.drain(..) {
            pending.push(super::partial_state::encode_partial_group(
                entry.key,
                entry.state.accumulators,
            ));
            if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
                partials
                    .push(Batch::new(schema.clone(), std::mem::take(&mut pending)))
                    .map_err(super::sort_fallback::exec_to_sql_error)?;
                pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
            }
        }
        self.groups = groups;
        if !pending.is_empty() {
            partials
                .push(Batch::new(schema, pending))
                .map_err(super::sort_fallback::exec_to_sql_error)?;
        }
        self.retained_bytes = 0;
        Ok(())
    }
}

fn matching_group_index(
    index: &GroupIndex,
    groups: &[GroupEntry],
    hash: u64,
    key: &[Value],
) -> Option<usize> {
    index.get(&hash).and_then(|bucket| {
        bucket
            .iter()
            .copied()
            .find(|group| groups[*group].key == key)
    })
}

pub(super) fn supports_adaptive_grouping(engine: &Engine, statement: &QueryBlockPlan) -> bool {
    if statement.group_by.is_empty() {
        return true;
    }
    aggregate_targets(engine, statement)
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

/// DISTINCT makes partial accumulator states non-mergeable, but a bounded
/// hash state is still the cheapest plan when it fits. The executor retains a
/// replay buffer and falls back to sort aggregation if this optimistic state
/// reaches its budget.
pub(super) fn supports_optimistic_grouping(engine: &Engine, statement: &QueryBlockPlan) -> bool {
    if statement.group_by.is_empty() {
        return false;
    }
    let targets = aggregate_targets(engine, statement);
    targets
        .iter()
        .any(|target| matches!(target, ScalarExpr::Func { distinct: true, .. }))
        && targets.into_iter().all(|target| match target {
            ScalarExpr::Func { name, order_by, .. } => {
                !engine.has_registered_aggregate_function(name)
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
                        .saturating_add(accumulator.distinct.memory_bytes)
                        .saturating_add(accumulator.values.memory_bytes)
                        .saturating_add(accumulator.registered_ordered.memory_bytes)
                })
                .sum::<usize>(),
        )
}

fn value_retained_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::Str(value) | Value::FixedChar(value) => value.capacity(),
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
