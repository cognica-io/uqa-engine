//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming aggregate state and registered aggregate adapters.

use super::state::{with_cleanup, RegisteredAggregateState};
use super::transitions::{increment, observe_extreme, GenericState, StatisticsState, SumState};
use super::{
    AggregateValueBuffer, Arc, DistinctTracker, RegisteredAggregateBuffer, SQLAggregateFactory,
    SQLAggregateState, SQLError, Value,
};
use std::{cmp::Ordering, num::NonZeroUsize, sync::LazyLock};
use uqa_sql::expr::enums::{EnumComparisonState, EnumLabelCatalog};

/// One group's active transition and its optional ordered or DISTINCT inputs.
pub struct AggregateAccumulator {
    pub(super) transition: Transition,
    pub(super) enum_comparison: Option<Arc<EnumComparisonState>>,
    component_budget: Option<NonZeroUsize>,
    buffers: Option<Box<AggregateBuffers>>,
}

pub(super) enum Transition {
    Generic(Box<GenericState>),
    Count(u64),
    Sum(SumState),
    Min(Option<Value>),
    Max(Option<Value>),
    BoolAnd(Option<bool>),
    BoolOr(Option<bool>),
    StringAgg(super::string_agg::Transition),
    Buffered,
    BufferedWithNulls,
    BufferedArrays(Box<super::array_inputs::ArrayInputShape>),
    Statistics(Box<StatisticsState>),
    Registered(Box<Registered>),
}

pub(super) struct Registered {
    factory: Arc<dyn SQLAggregateFactory>,
    state: RegisteredAggregateState,
    // Direct scalar observations use the generic builtin transition. Host callbacks receive argument tuples through observe_registered.
    scalar_input: Option<Box<GenericState>>,
}

#[derive(Default)]
pub(super) struct AggregateBuffers {
    pub(super) values: AggregateValueBuffer,
    pub(super) distinct: DistinctTracker,
    pub(super) registered_ordered: RegisteredAggregateBuffer,
}

#[derive(Clone, Copy, Default)]
pub enum NumericInputKind {
    #[default]
    Integers,
    Decimals,
    Floats,
    DecimalsAndFloats,
}

impl NumericInputKind {
    pub(super) fn observe_decimal(&mut self) {
        *self = match self {
            Self::Integers | Self::Decimals => Self::Decimals,
            Self::Floats | Self::DecimalsAndFloats => Self::DecimalsAndFloats,
        };
    }

    pub(super) fn observe_float(&mut self) {
        *self = match self {
            Self::Integers | Self::Floats => Self::Floats,
            Self::Decimals | Self::DecimalsAndFloats => Self::DecimalsAndFloats,
        };
    }

    pub(super) fn all_integers(self) -> bool {
        matches!(self, Self::Integers)
    }

    pub(super) fn decimal_without_float(self) -> bool {
        matches!(self, Self::Decimals)
    }

    pub(super) fn has_decimal(self) -> bool {
        matches!(self, Self::Decimals | Self::DecimalsAndFloats)
    }

    pub(super) fn has_float(self) -> bool {
        matches!(self, Self::Floats | Self::DecimalsAndFloats)
    }
}

#[derive(Clone, Copy)]
pub enum AggregateStatePlan {
    /// Conservative fallback for an aggregate whose state requirements
    /// are not known here.
    Generic,
    Count,
    Sum,
    SumReal,
    Min,
    Max,
    BoolAnd,
    BoolOr,
    /// Counts `StringInfo` appends before retaining unordered inputs. Ordered inputs reach the same transition after sorting.
    StringAgg,
    /// Retains non-NULL inputs: the ordered-set aggregates and the JSON object aggregates, whose pairs are never NULL.
    Buffered,
    /// Retains every input including NULL, as `array_agg` over non-array input, `json_agg` and `jsonb_agg` do.
    BufferedWithNulls,
    /// `array_agg` over array input, whose transition rejects NULL, empty and differently shaped arrays.
    BufferedArrays,
    Statistics,
}

impl AggregateStatePlan {
    pub(super) fn builtin_with_input_type(
        name: &str,
        input_type: Option<&uqa_sql::ast::ColumnType>,
    ) -> Self {
        use uqa_sql::ast::ColumnType;
        let name = uqa_sql::semantics::builtin_aggregate_dispatch_name(name);
        if let Some(ColumnType::Domain { base, .. }) = input_type {
            return Self::builtin_with_input_type(name, Some(base));
        }
        if name.eq_ignore_ascii_case("sum") && matches!(input_type, Some(ColumnType::Real)) {
            Self::SumReal
        } else if name.eq_ignore_ascii_case("array_agg")
            && matches!(input_type, Some(ColumnType::Array(_)))
        {
            Self::BufferedArrays
        } else {
            Self::builtin(name)
        }
    }

    pub(super) fn builtin(name: &str) -> Self {
        let name = uqa_sql::semantics::builtin_aggregate_dispatch_name(name);
        match name.to_ascii_lowercase().as_str() {
            "count" => Self::Count,
            "sum" | "avg" => Self::Sum,
            "min" => Self::Min,
            "max" => Self::Max,
            "bool_and" => Self::BoolAnd,
            "bool_or" => Self::BoolOr,
            "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
                Self::Statistics
            }
            "array_agg" | "json_agg" | "jsonb_agg" => Self::BufferedWithNulls,
            "string_agg" => Self::StringAgg,
            "json_object_agg" | "jsonb_object_agg" | "percentile_cont" | "percentile_disc"
            | "mode" => Self::Buffered,
            _ => Self::Generic,
        }
    }

    pub(super) fn retains_values(self) -> bool {
        matches!(
            self,
            Self::Generic
                | Self::StringAgg
                | Self::Buffered
                | Self::BufferedWithNulls
                | Self::BufferedArrays
        )
    }
}

impl Transition {
    pub(super) fn for_plan(plan: AggregateStatePlan) -> Self {
        match plan {
            AggregateStatePlan::Generic => Self::Generic(Box::default()),
            AggregateStatePlan::Count => Self::Count(0),
            AggregateStatePlan::Sum | AggregateStatePlan::SumReal => Self::Sum(SumState {
                real: matches!(plan, AggregateStatePlan::SumReal),
                ..SumState::default()
            }),
            AggregateStatePlan::Min => Self::Min(None),
            AggregateStatePlan::Max => Self::Max(None),
            AggregateStatePlan::BoolAnd => Self::BoolAnd(None),
            AggregateStatePlan::BoolOr => Self::BoolOr(None),
            AggregateStatePlan::StringAgg => {
                Self::StringAgg(super::string_agg::Transition::default())
            }
            AggregateStatePlan::Buffered => Self::Buffered,
            AggregateStatePlan::BufferedWithNulls => Self::BufferedWithNulls,
            AggregateStatePlan::BufferedArrays => Self::BufferedArrays(Box::default()),
            AggregateStatePlan::Statistics => Self::Statistics(Box::default()),
        }
    }

    pub(super) fn plan(&self) -> AggregateStatePlan {
        match self {
            Self::Generic(_) | Self::Registered(_) => AggregateStatePlan::Generic,
            Self::Count(_) => AggregateStatePlan::Count,
            Self::Sum(sum) if sum.real => AggregateStatePlan::SumReal,
            Self::Sum(_) => AggregateStatePlan::Sum,
            Self::Min(_) => AggregateStatePlan::Min,
            Self::Max(_) => AggregateStatePlan::Max,
            Self::BoolAnd(_) => AggregateStatePlan::BoolAnd,
            Self::BoolOr(_) => AggregateStatePlan::BoolOr,
            Self::StringAgg(_) => AggregateStatePlan::StringAgg,
            Self::Buffered => AggregateStatePlan::Buffered,
            Self::BufferedWithNulls => AggregateStatePlan::BufferedWithNulls,
            Self::BufferedArrays(_) => AggregateStatePlan::BufferedArrays,
            Self::Statistics(_) => AggregateStatePlan::Statistics,
        }
    }
}

impl Default for AggregateAccumulator {
    fn default() -> Self {
        Self::with_transition(Transition::for_plan(AggregateStatePlan::Generic), None)
    }
}

impl AggregateAccumulator {
    fn with_transition(transition: Transition, component_budget: Option<NonZeroUsize>) -> Self {
        Self {
            transition,
            component_budget,
            enum_comparison: None,
            buffers: None,
        }
    }

    pub(super) fn from_plan(plan: AggregateStatePlan) -> Self {
        Self::with_transition(Transition::for_plan(plan), None)
    }

    pub fn builtin(name: &str) -> Self {
        Self::from_plan(AggregateStatePlan::builtin(name))
    }

    pub fn builtin_with_input_type(
        name: &str,
        input_type: Option<&uqa_sql::ast::ColumnType>,
    ) -> Self {
        let mut accumulator = Self::from_plan(AggregateStatePlan::builtin_with_input_type(
            name, input_type,
        ));
        accumulator.enum_comparison = super::template::enum_extrema_state(name, input_type);
        accumulator
    }

    #[cfg(test)]
    pub(super) fn builtin_with_budget(name: &str, budget_bytes: usize) -> Self {
        Self::from_plan_with_budget(AggregateStatePlan::builtin(name), budget_bytes)
    }

    pub(super) fn from_plan_with_budget(plan: AggregateStatePlan, budget_bytes: usize) -> Self {
        Self::with_budget(Transition::for_plan(plan), budget_bytes)
    }

    pub(super) fn with_budget(transition: Transition, budget_bytes: usize) -> Self {
        Self::with_transition(transition, NonZeroUsize::new((budget_bytes / 2).max(1)))
    }

    pub(super) fn registered_with_budget(
        function: Arc<dyn SQLAggregateFactory>,
        budget_bytes: usize,
    ) -> Result<Self, SQLError> {
        let state = RegisteredAggregateState::new(function.try_create_state()?);
        Ok(Self::with_registered_state(function, state, budget_bytes))
    }

    fn with_registered_state(
        factory: Arc<dyn SQLAggregateFactory>,
        state: RegisteredAggregateState,
        budget_bytes: usize,
    ) -> Self {
        Self::with_budget(
            Transition::Registered(Box::new(Registered {
                factory,
                state,
                scalar_input: None,
            })),
            budget_bytes,
        )
    }

    pub(super) fn replay_with_budget(&self, budget_bytes: usize) -> Result<Self, SQLError> {
        match &self.transition {
            Transition::Registered(registered) => Ok(Self::with_registered_state(
                Arc::clone(&registered.factory),
                registered.state.replay(registered.factory.as_ref())?,
                budget_bytes,
            )),
            state => Ok(Self::from_plan_with_budget(state.plan(), budget_bytes)),
        }
    }

    pub(super) fn is_registered(&self) -> bool {
        matches!(self.transition, Transition::Registered(_))
    }

    pub(super) fn buffers(&self) -> &AggregateBuffers {
        static EMPTY: LazyLock<AggregateBuffers> = LazyLock::new(AggregateBuffers::default);
        self.buffers.as_deref().unwrap_or_else(|| &EMPTY)
    }

    pub(super) fn existing_buffers_mut(&mut self) -> Option<&mut AggregateBuffers> {
        self.buffers.as_deref_mut()
    }

    pub(super) fn buffers_mut(&mut self) -> &mut AggregateBuffers {
        self.buffers.get_or_insert_with(|| {
            Box::new(
                self.component_budget
                    .map_or_else(AggregateBuffers::default, |budget| AggregateBuffers {
                        values: AggregateValueBuffer::new(budget.get()),
                        distinct: DistinctTracker::new(budget.get()),
                        registered_ordered: RegisteredAggregateBuffer::new(budget.get()),
                    }),
            )
        })
    }

    pub(super) fn retained_bytes(&self) -> usize {
        use super::retained_size::payload_bytes;
        use super::DecimalValue;
        let decimal_bytes =
            |value: &Option<DecimalValue>| value.as_ref().map_or(0, DecimalValue::retained_bytes);
        let transition_bytes = match &self.transition {
            Transition::Generic(state) => state.retained_bytes(),
            Transition::Sum(state) => decimal_bytes(&state.decimal_sum),
            Transition::Min(value) | Transition::Max(value) => {
                value.as_ref().map_or(0, payload_bytes)
            }
            Transition::Statistics(state) => size_of::<StatisticsState>()
                .saturating_add(decimal_bytes(&state.origin))
                .saturating_add(decimal_bytes(&state.sum))
                .saturating_add(decimal_bytes(&state.sum_squares)),
            Transition::BufferedArrays(shape) => shape.retained_bytes(),
            Transition::Registered(registered) => size_of::<Registered>().saturating_add(
                registered
                    .scalar_input
                    .as_deref()
                    .map_or(0, GenericState::retained_bytes),
            ),
            _ => 0,
        };
        let buffer_bytes = self.buffers.as_deref().map_or(0, |buffers| {
            size_of::<AggregateBuffers>()
                .saturating_add(buffers.values.memory_bytes)
                .saturating_add(buffers.distinct.values.memory_bytes)
                .saturating_add(buffers.registered_ordered.memory_bytes)
        });
        size_of::<Self>()
            .saturating_add(transition_bytes)
            .saturating_add(buffer_bytes)
    }

    pub(super) fn count(&self) -> u64 {
        match &self.transition {
            Transition::Count(count) => *count,
            Transition::Sum(state) => state.count,
            Transition::Generic(state) => state.sum.count,
            _ => 0,
        }
    }

    pub(super) fn sum(&self) -> &SumState {
        static EMPTY: LazyLock<SumState> = LazyLock::new(SumState::default);
        match &self.transition {
            Transition::Sum(state) => state,
            Transition::Generic(state) => &state.sum,
            _ => &EMPTY,
        }
    }

    pub(super) fn min(&self) -> Option<&Value> {
        match &self.transition {
            Transition::Min(value) => value.as_ref(),
            Transition::Generic(state) => state.min.as_ref(),
            _ => None,
        }
    }

    pub(super) fn max(&self) -> Option<&Value> {
        match &self.transition {
            Transition::Max(value) => value.as_ref(),
            Transition::Generic(state) => state.max.as_ref(),
            _ => None,
        }
    }

    pub(super) fn bool_and(&self) -> Option<bool> {
        match &self.transition {
            Transition::BoolAnd(value) => *value,
            Transition::Generic(state) => state.bool_and,
            _ => None,
        }
    }

    pub(super) fn bool_or(&self) -> Option<bool> {
        match &self.transition {
            Transition::BoolOr(value) => *value,
            Transition::Generic(state) => state.bool_or,
            _ => None,
        }
    }

    pub(super) fn statistics(&self) -> &StatisticsState {
        static EMPTY: LazyLock<StatisticsState> = LazyLock::new(StatisticsState::default);
        match &self.transition {
            Transition::Statistics(state) => state,
            _ => &EMPTY,
        }
    }

    pub fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        self.observe_with_enum_catalog(value, None)
    }

    pub fn observe_with_enum_catalog(
        &mut self,
        value: &Value,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        if let Transition::BufferedArrays(shape) = &mut self.transition {
            shape.accept(value)?;
        }
        if matches!(value, Value::Null) {
            return self.observe_null(Vec::new(), enums);
        }
        self.observe_state(value, enums)?;
        if self.transition.plan().retains_values() {
            self.buffers_mut()
                .values
                .push(value.clone(), Vec::new(), enums)?;
        }
        Ok(())
    }

    pub(super) fn observe_projected(
        &mut self,
        value: &Value,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        match value {
            Value::Int(value) => self.observe_projected_integer(*value),
            _ => self.observe_with_enum_catalog(value, enums),
        }
    }

    pub(super) fn observe_projected_integer(&mut self, value: i64) -> Result<(), SQLError> {
        match &mut self.transition {
            Transition::Count(count) => increment(count),
            Transition::Sum(sum) => {
                increment(&mut sum.count)?;
                sum.add_integer(value)
            }
            _ => self.observe(&Value::Int(value)),
        }
    }

    fn observe_state(
        &mut self,
        value: &Value,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        // Decode lazy datums only for transitions that inspect their scalar contents.
        let decoded;
        let value = if matches!(value, Value::Datum(_))
            && matches!(
                self.transition,
                Transition::Generic(_)
                    | Transition::Registered(_)
                    | Transition::Sum(_)
                    | Transition::BoolAnd(_)
                    | Transition::BoolOr(_)
                    | Transition::Statistics(_)
            ) {
            decoded = super::observation::read(value, enums)?;
            &*decoded
        } else {
            value
        };
        let comparison = self.enum_comparison.as_deref();
        match &mut self.transition {
            Transition::Generic(generic) => generic.observe(value, enums, comparison),
            Transition::Count(count) => increment(count),
            Transition::Sum(sum) => {
                increment(&mut sum.count)?;
                sum.observe(value)
            }
            Transition::Min(current) => {
                observe_extreme(current, value, Ordering::Greater, enums, comparison)
            }
            Transition::Max(current) => {
                observe_extreme(current, value, Ordering::Less, enums, comparison)
            }
            Transition::BoolAnd(current) => {
                let Value::Bool(value) = value else {
                    return Err(SQLError::TypeMismatch(format!(
                        "BOOL_AND requires a boolean value, got {value:?}"
                    )));
                };
                *current = Some(current.unwrap_or(true) && *value);
                Ok(())
            }
            Transition::BoolOr(current) => {
                let Value::Bool(value) = value else {
                    return Err(SQLError::TypeMismatch(format!(
                        "BOOL_OR requires a boolean value, got {value:?}"
                    )));
                };
                *current = Some(current.unwrap_or(false) || *value);
                Ok(())
            }
            Transition::StringAgg(transition) => transition.accept(value, enums, |_| {}),
            Transition::Statistics(statistics) => statistics.observe(value),
            Transition::Buffered
            | Transition::BufferedWithNulls
            | Transition::BufferedArrays(_) => Ok(()),
            Transition::Registered(registered) => registered
                .scalar_input
                .get_or_insert_with(Box::default)
                .observe(value, enums, comparison),
        }
    }

    /// Ordered array and string inputs reach their transition functions in sorted order.
    pub(super) fn observe_with_sort_keys(
        &mut self,
        value: &Value,
        keys: Vec<super::ordering::AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return match self.transition.plan() {
                AggregateStatePlan::BufferedArrays => {
                    self.buffers_mut().values.push(Value::Null, keys, enums)
                }
                _ => self.observe_null(keys, enums),
            };
        }
        if self.transition.plan().retains_values()
            && !matches!(self.transition.plan(), AggregateStatePlan::StringAgg)
        {
            self.observe_state(value, enums)?;
        }
        self.buffers_mut().values.push(value.clone(), keys, enums)?;
        Ok(())
    }

    /// A NULL input is part of the aggregate's input only for aggregates that retain NULLs.
    fn observe_null(
        &mut self,
        keys: Vec<super::ordering::AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        match self.transition.plan() {
            AggregateStatePlan::BufferedWithNulls => {
                self.buffers_mut().values.push(Value::Null, keys, enums)
            }
            _ => Ok(()),
        }
    }

    /// Whether DISTINCT keeps a NULL input. Array accumulation rejects it when the transition sees it.
    pub(super) fn admits_null_input(&self) -> bool {
        matches!(
            self.transition.plan(),
            AggregateStatePlan::BufferedWithNulls | AggregateStatePlan::BufferedArrays
        )
    }

    pub(super) fn observe_registered(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<super::ordering::AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        let Transition::Registered(registered) = &mut self.transition else {
            return Err(SQLError::Internal(
                "registered aggregate state is missing".into(),
            ));
        };
        if sort_keys.is_empty() {
            return registered.state.observe(&values);
        }
        self.buffers_mut()
            .registered_ordered
            .push(values, sort_keys, enums)
    }

    pub(super) fn registered_value(
        &self,
        input: super::ordered_buffer::OrderedRows<'_, super::RegisteredAggregateRecord>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Option<Result<Value, SQLError>> {
        let Transition::Registered(registered) = &self.transition else {
            return None;
        };
        if input.buffer.next_sequence == 0 {
            return Some(registered.state.finish());
        }
        Some((|| {
            let mut state = registered.state.replay(registered.factory.as_ref())?;
            let result = input
                .observe_ordered_into(&mut state, enums)
                .and_then(|()| state.finish());
            with_cleanup(result, state.close())
        })())
    }

    /// Release host state once, including after a partial failure or early close.
    pub fn close(&mut self) -> Result<(), SQLError> {
        match &mut self.transition {
            Transition::Registered(registered) => registered.state.close(),
            _ => Ok(()),
        }
    }
}
