//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate call metadata and comparison support retained across groups, frames and spill restoration.

use super::{AggregateAccumulator, AggregateStatePlan, SQLAggregateFunction};
use std::sync::Arc;
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::enums::EnumComparisonState;

#[derive(Clone)]
pub struct AggregateAccumulatorTemplate {
    kind: AggregateKind,
    extrema: Option<Arc<EnumComparisonState>>,
    ordering: Option<Arc<[EnumComparisonState]>>,
}

#[derive(Clone)]
enum AggregateKind {
    Builtin(AggregateStatePlan),
    Registered(Arc<dyn SQLAggregateFunction>),
}

impl AggregateAccumulatorTemplate {
    pub(crate) fn builtin(name: &str, input_type: Option<&ColumnType>) -> Self {
        Self {
            kind: AggregateKind::Builtin(AggregateStatePlan::builtin_with_input_type(
                name, input_type,
            )),
            extrema: enum_extrema_state(name, input_type),
            ordering: None,
        }
    }

    pub(super) fn generic() -> Self {
        Self {
            kind: AggregateKind::Builtin(AggregateStatePlan::Generic),
            extrema: None,
            ordering: None,
        }
    }

    pub(super) fn registered(function: Arc<dyn SQLAggregateFunction>) -> Self {
        Self {
            kind: AggregateKind::Registered(function),
            extrema: None,
            ordering: None,
        }
    }

    pub(super) fn with_ordering(mut self, keys: usize) -> Self {
        if keys != 0 {
            self.ordering = Some((0..keys).map(|_| EnumComparisonState::default()).collect());
        }
        self
    }

    pub(super) fn state_plan(&self) -> Option<AggregateStatePlan> {
        match self.kind {
            AggregateKind::Builtin(plan) => Some(plan),
            AggregateKind::Registered(_) => None,
        }
    }

    pub(super) fn instantiate(&self, budget_bytes: usize) -> AggregateAccumulator {
        let mut accumulator = match &self.kind {
            AggregateKind::Builtin(plan) => {
                AggregateAccumulator::from_plan_with_budget(*plan, budget_bytes)
            }
            AggregateKind::Registered(function) => {
                AggregateAccumulator::registered_with_budget(Arc::clone(function), budget_bytes)
            }
        };
        self.restore_comparison(&mut accumulator);
        accumulator
    }

    pub(super) fn restore_comparison(&self, accumulator: &mut AggregateAccumulator) {
        accumulator.enum_comparison.clone_from(&self.extrema);
        accumulator
            .values
            .comparison_states
            .clone_from(&self.ordering);
        accumulator
            .distinct
            .values
            .comparison_states
            .clone_from(&self.ordering);
        accumulator
            .registered_ordered
            .comparison_states
            .clone_from(&self.ordering);
    }
}

pub(super) fn enum_extrema_state(
    name: &str,
    input_type: Option<&ColumnType>,
) -> Option<Arc<EnumComparisonState>> {
    if let Some(ColumnType::Domain { base, .. }) = input_type {
        return enum_extrema_state(name, Some(base));
    }
    ((name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max"))
        && matches!(input_type, Some(ColumnType::Enum(_))))
    .then(|| Arc::new(EnumComparisonState::default()))
}
