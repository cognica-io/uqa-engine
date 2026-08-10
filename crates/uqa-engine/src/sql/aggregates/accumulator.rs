//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming aggregate state and registered aggregate adapters.

use super::{
    value_as_f64, value_gt, value_lt, AggregateValueBuffer, Arc, DecimalValue, DistinctTracker,
    RegisteredAggregateBuffer, SQLAggregateFunction, SQLAggregateState, SQLError, Value,
};

pub(in crate::sql) struct AggregateAccumulator {
    pub(super) registered: Option<Arc<dyn SQLAggregateFunction>>,
    pub(super) registered_state: Option<Box<dyn SQLAggregateState>>,
    pub(super) registered_ordered: RegisteredAggregateBuffer,
    pub(super) count: u64,
    pub(super) sum: f64,
    pub(super) integer_sum: i128,
    pub(super) decimal_sum: Option<DecimalValue>,
    pub(super) numeric_inputs: NumericInputKind,
    pub(super) min: Option<Value>,
    pub(super) max: Option<Value>,
    /// Distinct-bookkeeping. Filled by the dispatcher when the
    /// aggregate was annotated with `DISTINCT`. Holds canonical-form
    /// keys so `Int(1)` and `Float(1.0)` collapse to the same bucket.
    pub(super) distinct: DistinctTracker,
    /// Only collection, ordered-set, and statistical aggregates need
    /// their complete input. Streaming aggregates keep constant-size
    /// state and must not spill values that their finalizer never reads.
    pub(super) state_plan: AggregateStatePlan,
    pub(super) values: AggregateValueBuffer,
    /// Boolean folds for `BOOL_AND` / `BOOL_OR`. Stay `None` until the
    /// first observation so an empty input set returns `NULL` (matches
    /// `PostgreSQL`).
    pub(super) bool_and: Option<bool>,
    pub(super) bool_or: Option<bool>,
    /// Welford state for variance/stddev. This avoids retaining the complete
    /// group for statistical aggregates.
    pub(super) statistics_count: u64,
    pub(super) statistics_mean: f64,
    pub(super) statistics_m2: f64,
    pub(super) statistics_has_float: bool,
}

#[derive(Clone)]
pub(in crate::sql) enum AggregateAccumulatorTemplate {
    Builtin(AggregateStatePlan),
    Registered(Arc<dyn SQLAggregateFunction>),
}

impl AggregateAccumulatorTemplate {
    pub(super) fn builtin(name: &str) -> Self {
        Self::Builtin(AggregateStatePlan::builtin(name))
    }

    pub(super) fn generic() -> Self {
        Self::Builtin(AggregateStatePlan::Generic)
    }

    pub(super) fn registered(function: Arc<dyn SQLAggregateFunction>) -> Self {
        Self::Registered(function)
    }

    pub(super) fn instantiate(&self, budget_bytes: usize) -> AggregateAccumulator {
        match self {
            Self::Builtin(state_plan) => {
                AggregateAccumulator::from_plan_with_budget(*state_plan, budget_bytes)
            }
            Self::Registered(function) => {
                AggregateAccumulator::registered_with_budget(Arc::clone(function), budget_bytes)
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(in crate::sql) enum NumericInputKind {
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
pub(in crate::sql) enum AggregateStatePlan {
    /// Conservative fallback for an aggregate whose state requirements
    /// are not known here.
    Generic,
    Count,
    Sum,
    Min,
    Max,
    BoolAnd,
    BoolOr,
    Buffered,
    Statistics,
}

impl AggregateStatePlan {
    pub(super) fn builtin(name: &str) -> Self {
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
            "string_agg" | "array_agg" | "json_agg" | "jsonb_agg" | "json_object_agg"
            | "jsonb_object_agg" | "percentile_cont" | "percentile_disc" | "mode" => Self::Buffered,
            _ => Self::Generic,
        }
    }

    pub(super) fn retains_values(self) -> bool {
        matches!(self, Self::Generic | Self::Buffered)
    }
}

impl Default for AggregateAccumulator {
    fn default() -> Self {
        Self {
            registered: None,
            registered_state: None,
            registered_ordered: RegisteredAggregateBuffer::default(),
            count: 0,
            sum: 0.0,
            integer_sum: 0,
            decimal_sum: None,
            numeric_inputs: NumericInputKind::default(),
            min: None,
            max: None,
            distinct: DistinctTracker::default(),
            state_plan: AggregateStatePlan::Generic,
            values: AggregateValueBuffer::default(),
            bool_and: None,
            bool_or: None,
            statistics_count: 0,
            statistics_mean: 0.0,
            statistics_m2: 0.0,
            statistics_has_float: false,
        }
    }
}

impl AggregateAccumulator {
    pub(super) fn with_budget(budget_bytes: usize) -> Self {
        let component_budget = (budget_bytes / 2).max(1);
        Self {
            registered: None,
            registered_state: None,
            registered_ordered: RegisteredAggregateBuffer::new(component_budget),
            count: 0,
            sum: 0.0,
            integer_sum: 0,
            decimal_sum: None,
            numeric_inputs: NumericInputKind::default(),
            min: None,
            max: None,
            distinct: DistinctTracker::new(component_budget),
            state_plan: AggregateStatePlan::Generic,
            values: AggregateValueBuffer::new(component_budget),
            bool_and: None,
            bool_or: None,
            statistics_count: 0,
            statistics_mean: 0.0,
            statistics_m2: 0.0,
            statistics_has_float: false,
        }
    }

    pub(in crate::sql) fn builtin(name: &str) -> Self {
        Self {
            state_plan: AggregateStatePlan::builtin(name),
            ..Self::default()
        }
    }

    pub(super) fn builtin_with_budget(name: &str, budget_bytes: usize) -> Self {
        Self::from_plan_with_budget(AggregateStatePlan::builtin(name), budget_bytes)
    }

    fn from_plan_with_budget(state_plan: AggregateStatePlan, budget_bytes: usize) -> Self {
        let mut accumulator = Self::with_budget(budget_bytes);
        accumulator.state_plan = state_plan;
        accumulator
    }

    pub(super) fn registered_with_budget(
        function: Arc<dyn SQLAggregateFunction>,
        budget_bytes: usize,
    ) -> Self {
        let state = function.create_state();
        let mut accumulator = Self::with_budget(budget_bytes);
        accumulator.registered = Some(function);
        accumulator.registered_state = Some(state);
        accumulator
    }

    pub(in crate::sql) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), Vec::new())?;
        }
        Ok(())
    }

    pub(super) fn observe_projected(&mut self, value: &Value) -> Result<(), SQLError> {
        match value {
            Value::Int(value) => self.observe_projected_integer(*value),
            _ => self.observe(value),
        }
    }

    pub(super) fn observe_projected_integer(&mut self, value: i64) -> Result<(), SQLError> {
        match self.state_plan {
            AggregateStatePlan::Count => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                Ok(())
            }
            AggregateStatePlan::Sum => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                self.integer_sum = self
                    .integer_sum
                    .checked_add(i128::from(value))
                    .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
                if self.numeric_inputs.has_decimal() {
                    let next = DecimalValue::from_i64(value);
                    self.decimal_sum = Some(
                        self.decimal_sum
                            .as_ref()
                            .and_then(|sum| sum.checked_add(&next))
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal aggregate overflow".into())
                            })?,
                    );
                }
                if self.numeric_inputs.has_float() {
                    self.sum += value as f64;
                }
                Ok(())
            }
            _ => self.observe(&Value::Int(value)),
        }
    }

    pub(super) fn observe_state(&mut self, value: &Value) -> Result<(), SQLError> {
        match self.state_plan {
            AggregateStatePlan::Generic => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                if matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
                    self.observe_sum(value)?;
                }
                self.observe_min(value);
                self.observe_max(value);
                if matches!(value, Value::Bool(_)) {
                    self.observe_bool_and(value)?;
                    self.observe_bool_or(value)?;
                }
            }
            AggregateStatePlan::Count => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
            }
            AggregateStatePlan::Sum => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                self.observe_sum(value)?;
            }
            AggregateStatePlan::Min => self.observe_min(value),
            AggregateStatePlan::Max => self.observe_max(value),
            AggregateStatePlan::BoolAnd => self.observe_bool_and(value)?,
            AggregateStatePlan::BoolOr => self.observe_bool_or(value)?,
            AggregateStatePlan::Buffered => {}
            AggregateStatePlan::Statistics => {
                self.statistics_has_float |= matches!(value, Value::Float(_));
                let value = value_as_f64(value)?;
                self.statistics_count = self.statistics_count.checked_add(1).ok_or_else(|| {
                    SQLError::TypeMismatch("statistical aggregate count overflow".into())
                })?;
                let count = self.statistics_count as f64;
                let delta = value - self.statistics_mean;
                self.statistics_mean += delta / count;
                let delta_after = value - self.statistics_mean;
                self.statistics_m2 += delta * delta_after;
            }
        }
        Ok(())
    }

    pub(super) fn observe_sum(&mut self, value: &Value) -> Result<(), SQLError> {
        if !matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
            return Err(SQLError::TypeMismatch(format!(
                "SUM/AVG requires a numeric value, got {value:?}"
            )));
        }
        match value {
            Value::Int(n) => {
                self.integer_sum = self
                    .integer_sum
                    .checked_add(i128::from(*n))
                    .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
                if self.numeric_inputs.has_decimal() {
                    let next = DecimalValue::from_i64(*n);
                    self.decimal_sum = Some(
                        self.decimal_sum
                            .as_ref()
                            .and_then(|sum| sum.checked_add(&next))
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal aggregate overflow".into())
                            })?,
                    );
                }
                if self.numeric_inputs.has_float() {
                    self.sum += *n as f64;
                }
            }
            Value::Decimal(d) => {
                let next = match &self.decimal_sum {
                    Some(sum) => sum.checked_add(d),
                    None if self.integer_sum == 0 => Some(d.clone()),
                    None => {
                        DecimalValue::from_i128(self.integer_sum).and_then(|sum| sum.checked_add(d))
                    }
                }
                .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?;
                self.decimal_sum = Some(next);
                if self.numeric_inputs.has_float() {
                    self.sum += d.to_f64().ok_or_else(|| {
                        SQLError::TypeMismatch("decimal aggregate does not fit float".into())
                    })?;
                }
                self.numeric_inputs.observe_decimal();
            }
            Value::Float(value) => {
                if !self.numeric_inputs.has_float() {
                    // Exact integer/decimal aggregates do not maintain a shadow
                    // floating total. Convert the accumulated total only if a
                    // floating input actually makes it necessary.
                    self.sum = if self.numeric_inputs.has_decimal() {
                        self.decimal_sum
                            .as_ref()
                            .and_then(DecimalValue::to_f64)
                            .ok_or_else(|| {
                                SQLError::TypeMismatch(
                                    "decimal aggregate does not fit float".into(),
                                )
                            })?
                    } else {
                        self.integer_sum as f64
                    };
                }
                self.sum += *value;
                self.numeric_inputs.observe_float();
            }
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "SUM/AVG requires a numeric value, got {value:?}"
                )))
            }
        }
        Ok(())
    }

    pub(super) fn observe_min(&mut self, value: &Value) {
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
    }

    pub(super) fn observe_max(&mut self, value: &Value) {
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
    }

    pub(super) fn observe_bool_and(&mut self, value: &Value) -> Result<(), SQLError> {
        let Value::Bool(value) = value else {
            return Err(SQLError::TypeMismatch(format!(
                "BOOL_AND requires a boolean value, got {value:?}"
            )));
        };
        self.bool_and = Some(self.bool_and.unwrap_or(true) && *value);
        Ok(())
    }

    pub(super) fn observe_bool_or(&mut self, value: &Value) -> Result<(), SQLError> {
        let Value::Bool(value) = value else {
            return Err(SQLError::TypeMismatch(format!(
                "BOOL_OR requires a boolean value, got {value:?}"
            )));
        };
        self.bool_or = Some(self.bool_or.unwrap_or(false) || *value);
        Ok(())
    }

    pub(super) fn observe_with_sort_keys(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), keys)?;
        }
        Ok(())
    }

    pub(super) fn observe_including_null(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        self.values.push(value.clone(), keys)
    }

    pub(super) fn observe_registered(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if sort_keys.is_empty() {
            let state = self
                .registered_state
                .as_mut()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()))?;
            state.observe(&values)?;
            return Ok(());
        }
        self.registered_ordered.push(values, sort_keys)
    }

    pub(super) fn registered_value(&self) -> Option<Result<Value, SQLError>> {
        let function = self.registered.as_ref()?;
        if self.registered_ordered.is_empty() {
            let state = self
                .registered_state
                .as_ref()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()));
            return Some(state.and_then(|state| state.finish()));
        }
        Some((|| {
            let mut state = function.create_state();
            self.registered_ordered
                .observe_ordered_into(state.as_mut())?;
            state.finish()
        })())
    }
}
