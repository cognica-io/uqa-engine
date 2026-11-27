//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adaptive streaming/hash aggregation with compact partial-state spill.

use crate::aggregation::value_retained_bytes;
use crate::RowSchemaExecution;
mod finish;
mod merge_keys;
mod projected;

use std::collections::HashMap;

use smallvec::SmallVec;
use uqa_sql::ast::ColumnType;

use super::{
    aggregate_accumulator_templates, aggregate_targets, eval_scalar,
    instantiate_aggregate_accumulators, AggregateAccumulator, AggregateAccumulatorTemplate,
    AggregateStatePlan, DecimalValue, PlanSubqueryArena, QueryBlockPlan, QueryExpressionContext,
    SQLError, SQLParam, ScalarEvalContext, ScalarExpr, SpillBuffer, Value,
};
#[cfg(test)]
use crate::hash_canonical_row;
use crate::{try_pack_compact_text_pair, Batch, RowSchema};

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
type CompactTextGroupIndex = HashMap<u64, usize, ahash::RandomState>;

pub(super) struct AdaptiveAggregateSet {
    group_by: Vec<ScalarExpr>,
    subqueries: std::sync::Arc<[uqa_sql::plan::QueryPlan]>,
    key_scratch: Vec<Value>,
    group_types: Vec<Option<ColumnType>>,
    group_expressions: crate::scalar::PreparedExpressions<Vec<ScalarExpr>>,
    aggregate_targets: crate::scalar::PreparedExpressions<Vec<ScalarExpr>>,
    accumulator_templates: Vec<AggregateAccumulatorTemplate>,
    output_plan: super::output::AggregateOutputPlan,
    state_budget: usize,
    spill_budget: usize,
    accumulator_budget: usize,
    groups: Vec<GroupEntry>,
    group_index: GroupIndex,
    compact_text_group_index: Option<CompactTextGroupIndex>,
    retained_bytes: usize,
    partials: Option<SpillBuffer>,
    partial_relation: uqa_sql::ast::InternalRelationId,
    variable_state: bool,
    projected_group_columns: Option<Vec<super::projected::ProjectedGroupColumn>>,
    projected_aggregate_plans: super::projected_input::ProjectedAggregatePlans,
    overflow_strategy: OverflowStrategy,
    abandoned: bool,
    order_group_values: bool,
}

impl AdaptiveAggregateSet {
    pub(super) fn close(&mut self) -> Result<(), SQLError> {
        let mut result = Ok(());
        for entry in &mut self.groups {
            result = super::state::with_cleanup(
                result,
                super::state::close_accumulators(&mut entry.state.accumulators),
            );
        }
        result
    }

    pub(super) fn new(
        context: &dyn QueryExpressionContext,
        statement: QueryBlockPlan,
        relaxed: bool,
        work_mem_bytes: usize,
        input_schema: &RowSchema,
        params: &[SQLParam],
        outer: Option<&super::AggregateOuterRow>,
    ) -> Result<Self, SQLError> {
        let aggregate_targets = aggregate_targets(context, &statement)
            .into_iter()
            .map(|target| {
                crate::bind_type_introspection_with_resolver(
                    target.clone(),
                    input_schema,
                    params,
                    context,
                )
            })
            .collect::<Vec<_>>();
        let accumulator_templates =
            aggregate_accumulator_templates(context, &aggregate_targets, input_schema, params)?;
        let output_plan = super::output::AggregateOutputPlan::compile(
            context,
            &statement,
            &aggregate_targets,
            relaxed,
            input_schema,
            params,
            outer,
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
                aggregate_target_has_variable_state(context, target, template, input_schema, params)
                    .map(|target_variable| variable || target_variable)
            })?;
        let projected_group_columns =
            super::projected::ProjectedGroupColumn::compile(&statement.group_by, input_schema);
        let compact_text_group_index = projected_group_columns
            .as_ref()
            .is_some_and(|columns| {
                columns.len() == 2
                    && columns.iter().all(|column| {
                        matches!(column, super::projected::ProjectedGroupColumn::Text(_))
                    })
            })
            .then(|| HashMap::with_hasher(ahash::RandomState::new()));
        let projected_aggregate_plans = super::projected_input::ProjectedAggregatePlans::compile(
            context,
            &aggregate_targets,
            input_schema,
        );
        let group_types = statement
            .group_by
            .iter()
            .map(|key| crate::scalar_type_with_resolver(key, input_schema, params, context))
            .collect::<Result<Vec<_>, _>>()?;
        let order_group_values = group_types_support_ordering(&group_types, context)?;
        Ok(Self {
            group_types,
            group_expressions: crate::scalar::PreparedExpressions::scalars(
                statement
                    .group_by
                    .iter()
                    .map(|expression| {
                        crate::bind_type_introspection_with_resolver(
                            expression.clone(),
                            input_schema,
                            params,
                            context,
                        )
                    })
                    .collect(),
            ),
            group_by: statement.group_by,
            subqueries: statement.subqueries.into(),
            key_scratch: Vec::new(),
            aggregate_targets: crate::scalar::PreparedExpressions::scalars(aggregate_targets),
            accumulator_templates,
            output_plan,
            state_budget,
            spill_budget,
            accumulator_budget,
            groups: Vec::new(),
            group_index: HashMap::with_hasher(ahash::RandomState::new()),
            compact_text_group_index,
            retained_bytes: 0,
            partials: None,
            partial_relation: uqa_sql::ast::InternalRelationId::allocate(),
            variable_state,
            projected_group_columns,
            projected_aggregate_plans,
            overflow_strategy: OverflowStrategy::SpillMergeablePartials,
            abandoned: false,
            order_group_values,
        })
    }

    /// Try hash aggregation for a non-mergeable DISTINCT state while the raw
    /// input is retained by the executor. If the hash state reaches its own
    /// budget, the executor drops it and replays the bounded input through the
    /// canonical sort aggregate.
    #[expect(
        clippy::too_many_arguments,
        reason = "preserves the same input scope through bounded hash-to-sort fallback"
    )]
    pub(super) fn new_optimistic(
        context: &dyn QueryExpressionContext,
        statement: QueryBlockPlan,
        relaxed: bool,
        state_budget: usize,
        output_budget: usize,
        input_schema: &RowSchema,
        params: &[SQLParam],
        outer: Option<&super::AggregateOuterRow>,
    ) -> Result<Self, SQLError> {
        let mut set = Self::new(
            context,
            statement,
            relaxed,
            state_budget,
            input_schema,
            params,
            outer,
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
        self.subqueries.is_empty()
    }

    pub(super) fn supports_storage_borrowed_rows(&self) -> bool {
        self.subqueries.is_empty()
            && self.projected_group_columns.is_some()
            && self.projected_aggregate_plans.all_direct()
    }

    pub(super) fn consume(
        &mut self,
        context: &dyn QueryExpressionContext,
        batch: &Batch,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        if self.abandoned {
            return Ok(());
        }
        let hook = context;
        let subqueries = std::sync::Arc::clone(&self.subqueries);
        let subquery_arena = PlanSubqueryArena::new(&subqueries, Some(hook));
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let context = ScalarEvalContext::from_row_lookup(&view, params)
                .with_row_schema(&batch.schema)
                .with_function_hook(hook)
                .with_subquery_runner(&subquery_arena)
                .with_physical_outer_row(&batch.schema, row);
            let Some(index) = self.group_for_row(&view, &context)? else {
                break;
            };
            if self
                .projected_group_columns
                .as_ref()
                .is_some_and(|columns| !columns.is_empty())
            {
                self.observe_projected(index, &view, &context)?;
            } else {
                super::sort_fallback::observe_targets(
                    &mut self.groups[index].state.accumulators,
                    &self.aggregate_targets,
                    &context.with_function_states(self.aggregate_targets.calls()),
                )?;
            }
            self.finish_observation(index)?;
            if self.abandoned {
                break;
            }
        }
        Ok(())
    }

    fn evaluated_group_index(
        &mut self,
        context: &ScalarEvalContext<'_>,
    ) -> Result<Option<usize>, SQLError> {
        self.key_scratch.clear();
        let group_context = (*context).with_function_states(self.group_expressions.calls());
        for expression in self.group_expressions.iter() {
            self.key_scratch
                .push(eval_scalar(expression, &group_context)?);
        }
        let enums = context
            .function_hook()
            .and_then(uqa_sql::expr::EngineHook::enum_labels);
        let hash = self.group_hash(&self.key_scratch, enums)?;
        if let Some(index) = matching_group_index(
            &self.group_index,
            &self.groups,
            hash,
            &self.key_scratch,
            &self.group_types,
            enums,
        )? {
            self.key_scratch.clear();
            return Ok(Some(index));
        }
        let key = std::mem::take(&mut self.key_scratch);
        self.insert_group(key, hash)
    }

    fn finish_observation(&mut self, index: usize) -> Result<(), SQLError> {
        if self.variable_state {
            let entry = &mut self.groups[index];
            let updated = estimate_group_bytes(&entry.key, &entry.state.accumulators);
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(entry.state.retained_bytes)
                .and_then(|bytes| bytes.checked_add(updated))
                .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
            entry.state.retained_bytes = updated;
        }
        self.handle_state_overflow()
    }

    fn group_hash(
        &self,
        key: &[Value],
        enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
    ) -> Result<u64, SQLError> {
        if let Some(key) = self.compact_text_group_key(key) {
            return Ok(key);
        }
        crate::distinct::hash_typed_row_with_catalog(
            self.group_index.hasher(),
            key.iter().enumerate().map(|(index, value)| {
                (
                    Some(value),
                    self.group_types.get(index).and_then(Option::as_ref),
                )
            }),
            enums,
        )
        .map_err(super::sort_fallback::exec_to_sql_error)
    }

    fn compact_text_group_key(&self, key: &[Value]) -> Option<u64> {
        let columns = self.projected_group_columns.as_ref()?;
        if columns.len() != key.len()
            || !columns
                .iter()
                .all(|column| matches!(column, super::projected::ProjectedGroupColumn::Text(_)))
        {
            return None;
        }
        try_pack_compact_text_pair(key.iter().map(Some))
    }

    fn insert_group(&mut self, key: Vec<Value>, hash: u64) -> Result<Option<usize>, SQLError> {
        if self.abandoned {
            return Ok(None);
        }
        let mut accumulators = instantiate_aggregate_accumulators(
            &self.accumulator_templates,
            self.accumulator_budget,
        )?;
        let group_bytes = estimate_group_bytes(&key, &accumulators);
        if !self.group_by.is_empty()
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
                    return Ok(None);
                }
            }
        }
        let compact_text_key = self.compact_text_group_key(&key);
        let Some(retained_bytes) = self.retained_bytes.checked_add(group_bytes) else {
            return super::state::with_cleanup(
                Err(SQLError::Internal("aggregate state size overflow".into())),
                super::state::close_accumulators(&mut accumulators),
            );
        };
        self.retained_bytes = retained_bytes;
        let index = self.groups.len();
        self.groups.push(GroupEntry {
            key,
            state: GroupState {
                accumulators,
                retained_bytes: group_bytes,
            },
        });
        self.group_index.entry(hash).or_default().push(index);
        if let (Some(key), Some(compact_index)) =
            (compact_text_key, self.compact_text_group_index.as_mut())
        {
            compact_index.insert(key, index);
        }
        Ok(Some(index))
    }

    fn ungrouped_index(&mut self) -> Result<Option<usize>, SQLError> {
        match self.groups.len() {
            0 => {
                let hash = self.group_hash(&[], None)?;
                self.insert_group(Vec::new(), hash)
            }
            1 => Ok(Some(0)),
            _ => Err(SQLError::Internal(
                "ungrouped aggregate retained more than one group".into(),
            )),
        }
    }

    fn handle_state_overflow(&mut self) -> Result<(), SQLError> {
        if self.group_by.is_empty() || self.retained_bytes <= self.state_budget {
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
        if let Some(index) = self.compact_text_group_index.as_mut() {
            index.clear();
        }
        self.partials = None;
        self.retained_bytes = 0;
        self.abandoned = true;
    }

    fn flush_partial_groups(&mut self) -> Result<(), SQLError> {
        if self.groups.is_empty() {
            return Ok(());
        }
        let schema = super::partial_state::partial_schema(self.partial_relation, &self.group_types);
        let partials = self
            .partials
            .get_or_insert_with(|| SpillBuffer::new(self.spill_budget));
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        let mut groups = std::mem::take(&mut self.groups);
        self.group_index.clear();
        if let Some(index) = self.compact_text_group_index.as_mut() {
            index.clear();
        }
        for entry in groups.drain(..) {
            pending.push(super::partial_state::encode_partial_group(
                entry.key,
                entry.state.accumulators,
            ));
            if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
                partials
                    .push(Batch::from_physical_rows(
                        schema.clone(),
                        std::mem::take(&mut pending),
                    ))
                    .map_err(super::sort_fallback::exec_to_sql_error)?;
                pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
            }
        }
        self.groups = groups;
        if !pending.is_empty() {
            partials
                .push(Batch::from_physical_rows(schema, pending))
                .map_err(super::sort_fallback::exec_to_sql_error)?;
        }
        self.retained_bytes = 0;
        Ok(())
    }
}

fn group_types_support_ordering(
    types: &[Option<ColumnType>],
    context: &dyn QueryExpressionContext,
) -> Result<bool, SQLError> {
    types.iter().try_fold(true, |available, ty| {
        Ok(available
            && ty
                .as_ref()
                .map(|ty| uqa_sql::type_resolution::type_supports_ordering(ty, context))
                .transpose()?
                .unwrap_or(true))
    })
}

fn matching_group_index(
    index: &GroupIndex,
    groups: &[GroupEntry],
    hash: u64,
    key: &[Value],
    types: &[Option<ColumnType>],
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> Result<Option<usize>, SQLError> {
    let Some(bucket) = index.get(&hash) else {
        return Ok(None);
    };
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    for group in bucket {
        let candidate = &groups[*group].key;
        if candidate.len() != key.len() {
            continue;
        }
        let mut equal = true;
        for (position, (left, right)) in candidate.iter().zip(key).enumerate() {
            if !uqa_sql::expr::equal_selected_values_with_catalog(
                left,
                right,
                types.get(position).and_then(Option::as_ref),
                &control,
                enums,
            )? {
                equal = false;
                break;
            }
        }
        if equal {
            return Ok(Some(*group));
        }
    }
    Ok(None)
}

pub(super) fn supports_adaptive_grouping(
    context: &dyn QueryExpressionContext,
    statement: &QueryBlockPlan,
) -> bool {
    if statement.group_by.is_empty() {
        return true;
    }
    aggregate_targets(context, statement)
        .into_iter()
        .all(|target| match target {
            ScalarExpr::Func {
                name,
                distinct,
                order_by,
                ..
            } => {
                !context.is_registered_aggregate(name)
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
pub(super) fn supports_optimistic_grouping(
    context: &dyn QueryExpressionContext,
    statement: &QueryBlockPlan,
) -> bool {
    if statement.group_by.is_empty() {
        return false;
    }
    let targets = aggregate_targets(context, statement);
    targets
        .iter()
        .any(|target| matches!(target, ScalarExpr::Func { distinct: true, .. }))
        && targets.into_iter().all(|target| match target {
            ScalarExpr::Func { name, order_by, .. } => {
                !context.is_registered_aggregate(name)
                    && order_by.is_empty()
                    && is_mergeable_builtin(name)
            }
            _ => false,
        })
}

fn is_mergeable_builtin(name: &str) -> bool {
    let name = uqa_sql::semantics::builtin_aggregate_dispatch_name(name);
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
    context: &dyn QueryExpressionContext,
    target: &ScalarExpr,
    template: &AggregateAccumulatorTemplate,
    input_schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let Some(plan) = template.state_plan() else {
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
        AggregateStatePlan::Sum | AggregateStatePlan::SumReal => {
            let Some(argument) = args.first() else {
                return Ok(true);
            };
            let input_type =
                crate::scalar_type_with_resolver(argument, input_schema, params, context)?;
            Ok(!input_type.as_ref().is_some_and(fixed_width_sum_type))
        }
        AggregateStatePlan::Generic
        | AggregateStatePlan::Min
        | AggregateStatePlan::Max
        | AggregateStatePlan::Buffered
        | AggregateStatePlan::StringAgg
        | AggregateStatePlan::BufferedWithNulls
        | AggregateStatePlan::BufferedArrays
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
                        .saturating_add(accumulator.distinct.values.memory_bytes)
                        .saturating_add(accumulator.values.memory_bytes)
                        .saturating_add(accumulator.registered_ordered.memory_bytes)
                })
                .sum::<usize>(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aggregate(sql: &str, budget: usize) -> AdaptiveAggregateSet {
        use uqa_sql::plan::{RelationalPlan, UnifiedPlan};
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("expected a query");
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("expected a query block")
        };
        AdaptiveAggregateSet::new(
            &crate::query::relational::values::test_support::Services,
            *block,
            false,
            budget,
            &RowSchema::default(),
            &[],
            None,
        )
        .unwrap()
    }

    #[test]
    fn evaluated_keys_reuse_their_slots_and_new_groups_take_key_ownership() {
        let mut set = aggregate("SELECT count(*) GROUP BY true", 1 << 20);
        let row = crate::OwnedPhysicalRow::new(
            RowSchema::default(),
            crate::PhysicalRow::from_values(Vec::new()),
        );
        let context = ScalarEvalContext::from_row_lookup(&row, &[]).with_row_schema(&row.schema);
        let key = vec![Value::Bool(true)];
        let pointer = key.as_ptr();
        let hash = set.group_hash(&key, None).unwrap();
        assert_eq!(set.insert_group(key, hash).unwrap(), Some(0));
        assert_eq!(set.groups[0].key.as_ptr(), pointer);
        assert_eq!(set.group_for_row(&row, &context).unwrap(), Some(0));
        let slots = set.key_scratch.as_ptr();
        assert!(set.key_scratch.capacity() >= 1);
        let allocations = allocation_counter::measure(|| {
            for _ in 0..10_000 {
                assert_eq!(set.group_for_row(&row, &context).unwrap(), Some(0));
            }
        });
        assert_eq!(set.key_scratch.as_ptr(), slots);
        assert_eq!(allocations.count_total, 0);
    }

    #[test]
    fn batches_reuse_subquery_plans_without_allocating() {
        let mut set = aggregate("SELECT count(*) HAVING (SELECT 1) = 1", 1 << 20);
        assert_eq!(set.subqueries.len(), 1);
        let empty = Batch::from_physical_rows(RowSchema::default(), Vec::new());
        let allocations = allocation_counter::measure(|| {
            for _ in 0..10_000 {
                set.consume(
                    &crate::query::relational::values::test_support::Services,
                    &empty,
                    &[],
                )
                .unwrap();
            }
        });
        assert_eq!(allocations.count_total, 0);
    }

    #[test]
    fn datum_hash_buckets_compare_decoded_sql_values() {
        let old = Value::Record(
            vec![(
                "a".into(),
                Value::Datum(uqa_core::DatumValue::new(
                    17,
                    0,
                    vec![13, b'h', b'e', b'l', b'l', b'o'],
                )),
            )]
            .into(),
        );
        let fresh = Value::Record(vec![("a".into(), Value::Bytes(b"hello".to_vec()))].into());
        let mut index = GroupIndex::with_hasher(ahash::RandomState::new());
        let hash = hash_canonical_row(index.hasher(), [Some(&old)].into_iter()).unwrap();
        assert_eq!(
            hash,
            hash_canonical_row(index.hasher(), [Some(&fresh)].into_iter()).unwrap()
        );
        index.insert(hash, SmallVec::from_slice(&[0]));
        let groups = vec![GroupEntry {
            key: vec![old],
            state: GroupState {
                accumulators: Vec::new(),
                retained_bytes: 0,
            },
        }];
        assert_eq!(
            matching_group_index(&index, &groups, hash, &[fresh], &[], None).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn row_group_accounting_includes_the_header_and_descriptor_capacity() {
        let mut fields = Vec::with_capacity(64);
        fields.push(uqa_core::RecordFieldType {
            collation_oid: None,
            oid: 23,
            type_modifier: -1,
        });
        // Immutable descriptors discard the temporary vector's spare slots.
        let descriptor_bytes = fields.len() * std::mem::size_of::<uqa_core::RecordFieldType>();
        let row = Value::Row(uqa_core::RowValue::typed(vec![Value::Null], fields).unwrap());
        let untyped = Value::Row(vec![Value::Null].into());
        assert_eq!(
            value_retained_bytes(&row),
            value_retained_bytes(&untyped) + descriptor_bytes,
        );
        assert_eq!(
            value_retained_bytes(&Value::Row(Vec::new().into())),
            std::mem::size_of::<Value>() + uqa_core::RowValue::retained_header_bytes(),
        );
    }

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
