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
use uqa_sql::ast::ColumnType;

use super::{
    aggregate_accumulator_templates, aggregate_targets, eval_scalar,
    instantiate_aggregate_accumulators, AggregateAccumulator, AggregateAccumulatorTemplate,
    AggregateStatePlan, CteScope, DecimalValue, Engine, PlanSubqueryArena, QueryBlockPlan,
    SQLError, SQLParam, ScalarEvalContext, ScalarExpr, ScopedEngineHook, SpillBuffer, Value,
};
use uqa_execution::{hash_canonical_row, Batch, RowSchema};

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
        input_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Self, SQLError> {
        let aggregate_targets = aggregate_targets(engine, &statement)
            .into_iter()
            .map(|target| {
                uqa_execution::bind_type_introspection_with_resolver(
                    target.clone(),
                    input_schema,
                    params,
                    engine,
                )
            })
            .collect::<Vec<_>>();
        let accumulator_templates = aggregate_accumulator_templates(engine, &aggregate_targets);
        let output_plan = super::output::AggregateOutputPlan::compile(
            engine,
            &statement,
            &aggregate_targets,
            relaxed,
            input_schema,
            params,
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
            .zip(&accumulator_templates)
            .try_fold(false, |variable, (target, template)| {
                aggregate_target_has_variable_state(engine, target, template, input_schema, params)
                    .map(|target_variable| variable || target_variable)
            })?;
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
        input_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Self, SQLError> {
        let mut set = Self::new(
            engine,
            statement,
            relaxed,
            state_budget,
            input_schema,
            params,
        )?;
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
                .with_subquery_runner(&subquery_arena)
                .with_physical_outer_row(&batch.schema, row);
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

fn aggregate_target_has_variable_state(
    engine: &Engine,
    target: &ScalarExpr,
    template: &AggregateAccumulatorTemplate,
    input_schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let AggregateAccumulatorTemplate::Builtin(plan) = template else {
        return Ok(true);
    };
    let ScalarExpr::Func {
        args,
        distinct,
        order_by,
        ..
    } = target
    else {
        return Ok(true);
    };
    if *distinct || !order_by.is_empty() {
        return Ok(true);
    }
    match plan {
        AggregateStatePlan::Count | AggregateStatePlan::BoolAnd | AggregateStatePlan::BoolOr => {
            Ok(false)
        }
        AggregateStatePlan::Sum => {
            let Some(argument) = args.first() else {
                return Ok(true);
            };
            let input_type =
                uqa_execution::scalar_type_with_resolver(argument, input_schema, params, engine)?;
            Ok(!input_type.as_ref().is_some_and(fixed_width_sum_type))
        }
        AggregateStatePlan::Generic
        | AggregateStatePlan::Min
        | AggregateStatePlan::Max
        | AggregateStatePlan::Buffered
        | AggregateStatePlan::Statistics => Ok(true),
    }
}

fn fixed_width_sum_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Real
        | ColumnType::DoublePrecision => true,
        ColumnType::Domain { base, .. } => fixed_width_sum_type(base),
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
                        .saturating_add(
                            accumulator
                                .decimal_sum
                                .as_ref()
                                .map_or(0, DecimalValue::retained_bytes),
                        )
                        .saturating_add(accumulator.min.as_ref().map_or(0, value_retained_bytes))
                        .saturating_add(accumulator.max.as_ref().map_or(0, value_retained_bytes))
                        .saturating_add(
                            accumulator
                                .statistics_origin
                                .as_ref()
                                .map_or(0, DecimalValue::retained_bytes),
                        )
                        .saturating_add(
                            accumulator
                                .statistics_sum
                                .as_ref()
                                .map_or(0, DecimalValue::retained_bytes),
                        )
                        .saturating_add(
                            accumulator
                                .statistics_sum_squares
                                .as_ref()
                                .map_or(0, DecimalValue::retained_bytes),
                        )
                        .saturating_add(accumulator.distinct.memory_bytes)
                        .saturating_add(accumulator.values.memory_bytes)
                        .saturating_add(accumulator.registered_ordered.memory_bytes)
                })
                .sum::<usize>(),
        )
}

fn value_retained_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::Str(value) | Value::FixedChar(value) | Value::Json(value) | Value::JsonB(value) => {
            value.capacity()
        }
        Value::Bytes(value) => value.capacity(),
        Value::Array(array) => array
            .elements()
            .len()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(array.elements().iter().map(value_retained_bytes).sum())
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
    fn exact_decimal_aggregate_allocations_are_charged_to_group_state() {
        assert!(DecimalValue::from_i64(1).retained_bytes() >= std::mem::size_of::<usize>());
        let value = DecimalValue::parse("1e20000").unwrap();
        let mut statistics = AggregateAccumulator::builtin("var_pop");
        let empty_statistics = estimate_group_bytes(&[], std::slice::from_ref(&statistics));
        statistics.observe(&Value::Decimal(value.clone())).unwrap();
        assert!(
            estimate_group_bytes(&[], std::slice::from_ref(&statistics))
                > empty_statistics.saturating_add(4_000)
        );

        let mut sum = AggregateAccumulator::builtin("sum");
        let empty_sum = estimate_group_bytes(&[], std::slice::from_ref(&sum));
        sum.observe(&Value::Decimal(value.clone())).unwrap();
        assert!(
            estimate_group_bytes(&[], std::slice::from_ref(&sum)) > empty_sum.saturating_add(4_000)
        );

        assert!(value_retained_bytes(&Value::Decimal(value)) > std::mem::size_of::<Value>());
        assert!(fixed_width_sum_type(&ColumnType::BigInteger));
        assert!(fixed_width_sum_type(&ColumnType::DoublePrecision));
        assert!(!fixed_width_sum_type(&ColumnType::Numeric {
            precision: None,
            scale: None,
        }));
    }
}
