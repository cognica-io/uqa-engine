//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Flat plan metadata retires its native scalar roots before ordinary field destruction.

use crate::{
    ast::{
        AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets,
        WindowDefinition,
    },
    plan::{
        AssignmentPlan, ConflictActionPlan, ConflictPlan, CteCyclePlan, MergeWhenPlan, OrderPlan,
        ProjectionPlan, QueryBlockPlan, SourcePlan, TableFunctionPlan, ViewCheckPlan,
    },
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};

pub(crate) trait Retire {
    fn retire(&mut self);
}

// Structural source children belong to the enclosing retained plan tree; this operation retires only the source's local scalar roots.
impl Retire for SourcePlan {
    fn retire(&mut self) {
        match self {
            Self::Table { .. } | Self::Subquery { .. } | Self::RetainedRows(_) => {}
            Self::Join { on, .. } => on.retire(),
            Self::Values { rows, .. } => rows.retire(),
            Self::Function { args, .. } => args.retire(),
            Self::FunctionGroup { functions, .. } => functions.retire(),
        }
    }
}

impl Retire for ScalarExpr {
    fn retire(&mut self) {
        crate::ir::ownership::retire(self);
    }
}
impl<T: Retire> Retire for Vec<T> {
    fn retire(&mut self) {
        for value in self {
            value.retire();
        }
    }
}
impl<T: Retire> Retire for uqa_core::memory::BudgetedVec<T> {
    fn retire(&mut self) {
        for value in self.iter_mut() {
            value.retire();
        }
    }
}
impl<T: Retire> Retire for Option<T> {
    fn retire(&mut self) {
        if let Some(value) = self {
            value.retire();
        }
    }
}
impl<T: Retire> Retire for Box<T> {
    fn retire(&mut self) {
        self.as_mut().retire();
    }
}

macro_rules! scalar_fields {
    ($ty:ty { $($field:ident),* $(,)? } $(skip { $($skip:ident),* $(,)? })?) => {
        impl Retire for $ty {
            fn retire(&mut self) {
                let Self { $($field,)* $($($skip: _,)*)? } = self;
                $($field.retire();)*
            }
        }
    };
}

scalar_fields!(ProjectionPlan { expr } skip { alias });
scalar_fields!(TableFunctionPlan { args } skip { name, binding, output_name, relations, column_aliases, column_types });
scalar_fields!(OrderPlan { expr } skip { descending, nulls });
scalar_fields!(ScalarOrder { expr } skip { descending, nulls });
scalar_fields!(ScalarWindowSpec { partition_by, order_by, frame } skip { definition });
scalar_fields!(ScalarWindowFrame { start, end } skip { mode, between, exclusion });
scalar_fields!(WindowDefinition<ScalarWindowSpec> { spec } skip { name, inherited });
scalar_fields!(AssignmentTarget<ScalarExpr> { indirection } skip { column });
scalar_fields!(MultipleAssignmentTargets<ScalarExpr> { targets } skip { source_positions, source_width });
scalar_fields!(AssignmentPlan { target, value });
scalar_fields!(ConflictPlan { predicate, expressions, action } skip { constraint, conflict_columns });
scalar_fields!(ViewCheckPlan { predicate } skip { view });
scalar_fields!(CteCyclePlan { mark_value, mark_default } skip { columns, mark_column, path_column });

impl Retire for QueryBlockPlan {
    fn retire(&mut self) {
        self.windows.retire();
        self.projections.retire();
        self.r#where.retire();
        self.group_by.retire();
        self.grouping_sets.retire();
        self.having.retire();
        self.order_by.retire();
        self.limit.retire();
        self.offset.retire();
        self.distinct_on.retire();
    }
}

impl Retire for ScalarFrameBound {
    fn retire(&mut self) {
        match self {
            Self::Preceding(value) | Self::Following(value) => value.retire(),
            Self::UnboundedPreceding | Self::UnboundedFollowing | Self::CurrentRow => {}
        }
    }
}
impl Retire for AssignmentTargets<ScalarExpr> {
    fn retire(&mut self) {
        match self {
            Self::Single(value) => value.retire(),
            Self::Multiple(value) => value.retire(),
        }
    }
}
impl Retire for AssignmentStep<ScalarExpr> {
    fn retire(&mut self) {
        match self {
            Self::Field(_) => {}
            Self::Index(value) => value.retire(),
            Self::Slice { lower, upper } => {
                lower.retire();
                upper.retire();
            }
        }
    }
}
impl Retire for ConflictActionPlan {
    fn retire(&mut self) {
        match self {
            Self::Nothing => {}
            Self::Update {
                assignments,
                predicate,
            } => {
                assignments.retire();
                predicate.retire();
            }
        }
    }
}
impl Retire for MergeWhenPlan {
    fn retire(&mut self) {
        match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                condition.retire();
                assignments.retire();
            }
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => condition.retire(),
            Self::InsertNotMatched {
                condition,
                columns,
                overriding: _,
                values,
            } => {
                condition.retire();
                columns.retire();
                values.retire();
            }
        }
    }
}

/// A field being read owns its scalar descendants until an infallible move into its admitted native parent.
pub(crate) struct Pending<T: Retire>(Option<T>);

impl<T: Retire> Pending<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Some(value))
    }
    pub(crate) fn take(mut self) -> T {
        self.0.take().expect("pending native field")
    }

    pub(crate) fn finish(
        self,
        memory: Option<uqa_core::memory::MemoryReservation>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<T>, uqa_core::ValueRetentionError> {
        let value = control.finish(self, memory)?;
        match value.into_budgeted() {
            Ok(value) => {
                // The final cancellation check ran while retirement ownership was live. Only an infallible move remains.
                Ok(value
                    .try_map(|value| Ok::<_, uqa_core::ValueRetentionError>(value.take()))?
                    .into())
            }
            Err(value) => {
                let value = value
                    .into_uncontrolled()
                    .unwrap_or_else(|_| unreachable!("ordinary scalar field publication"));
                Ok(control.finish(value.take(), None)?)
            }
        }
    }
}
impl<T: Retire> std::ops::Deref for Pending<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("pending native field")
    }
}
impl<T: Retire> std::ops::DerefMut for Pending<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("pending native field")
    }
}
impl<T: Retire> Drop for Pending<T> {
    fn drop(&mut self) {
        self.0.retire();
    }
}
