//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native plan fields retain their declared order across immediate and stepped retirement.

use crate::{
    ast::{
        AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets,
        WindowDefinition,
    },
    ir::ownership::Retirement,
    plan::{
        AssignmentPlan, ConflictActionPlan, ConflictPlan, CteCyclePlan, MergeWhenPlan, OrderPlan,
        ProjectionPlan, QueryBlockPlan, SourcePlan, TableFunctionPlan, ViewCheckPlan,
    },
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};

mod steps;
pub(crate) use steps::ScalarRetirement;
mod metadata;

pub(crate) trait Retire: Sized {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool;
    fn retire(&mut self) {
        let mut retirement = ScalarRetirement::default();
        while !retirement.advance(self) {}
    }
}

macro_rules! fields {
    ($positions:expr, $scalar:expr; $($field:expr),* $(,)?) => {
        true $(&& $field.retire_step($positions, $scalar))*
    };
}
pub(crate) use fields;

impl Retire for SourcePlan {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                ..
            } => {
                fields!(positions, scalar; name, qualifier, alias, column_aliases, bound_columns)
            }
            Self::Join {
                on,
                using,
                alias,
                column_aliases,
                ..
            } => {
                fields!(positions, scalar; on, using, alias, column_aliases)
            }
            Self::Values {
                rows,
                alias,
                column_aliases,
                internal_column_types,
                ..
            } => {
                fields!(positions, scalar; rows, alias, column_aliases, internal_column_types)
            }
            Self::Function {
                args,
                name,
                binding,
                output_name,
                relations,
                alias,
                column_aliases,
                column_types,
                ..
            } => {
                fields!(positions, scalar; args, name, binding, output_name, relations, alias, column_aliases, column_types)
            }
            Self::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ..
            } => {
                fields!(positions, scalar; functions, alias, column_aliases)
            }
            Self::Subquery {
                alias,
                column_aliases,
                ..
            } => {
                fields!(positions, scalar; alias, column_aliases)
            }
            Self::RetainedRows(_) => true,
        }
    }
}

impl Retire for ScalarExpr {
    fn retire_step(&mut self, _: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        if matches!(self, Self::Star) {
            return true;
        }
        *scalar = Some(Retirement::new(std::mem::replace(self, Self::Star)));
        false
    }
}

macro_rules! vector {
    ($ty:ty) => {
        impl<T: Retire> Retire for $ty {
            fn retire_step(
                &mut self,
                positions: &mut [usize],
                scalar: &mut Option<Retirement>,
            ) -> bool {
                if self.is_empty() {
                    return true;
                }
                let (position, children) = positions
                    .split_first_mut()
                    .expect("native scalar container depth");
                if let Some(value) = self.get_mut(*position) {
                    if value.retire_step(children, scalar) {
                        *position += 1;
                    }
                    return false;
                }
                if self.pop().is_some() {
                    if self.is_empty() {
                        *position = 0;
                    }
                    return false;
                }
                unreachable!("nonempty scalar container")
            }
        }
    };
}
vector!(Vec<T>);
vector!(uqa_core::memory::BudgetedVec<T>);

impl<T: Retire> Retire for Option<T> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.as_mut()
            .is_none_or(|value| value.retire_step(positions, scalar))
    }
}
impl<T: Retire> Retire for Box<T> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.as_mut().retire_step(positions, scalar)
    }
}
impl<T: Retire, const N: usize> Retire for [T; N] {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.iter_mut()
            .all(|value| value.retire_step(positions, scalar))
    }
}
impl<A: Retire, B: Retire> Retire for (A, B) {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        fields!(positions, scalar; self.0, self.1)
    }
}

macro_rules! scalar_fields {
    ($ty:ty { $($field:ident),* $(,)? } $(skip { $($skip:ident),* $(,)? })?) => {
        impl Retire for $ty {
            fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
                let Self { $($field,)* $($($skip: _,)*)? } = self;
                fields!(positions, scalar; $($field),*)
            }
        }
    };
}
pub(super) use scalar_fields;

scalar_fields!(ProjectionPlan { expr, alias });
scalar_fields!(TableFunctionPlan {
    args,
    name,
    binding,
    output_name,
    relations,
    column_aliases,
    column_types
});
scalar_fields!(OrderPlan { expr } skip { descending, nulls });
scalar_fields!(ScalarOrder { expr } skip { descending, nulls });
scalar_fields!(ScalarWindowSpec { partition_by, order_by, frame } skip { definition });
scalar_fields!(ScalarWindowFrame { start, end } skip { mode, between, exclusion });
scalar_fields!(WindowDefinition<ScalarWindowSpec> { spec, name } skip { inherited });
scalar_fields!(AssignmentTarget<ScalarExpr> { indirection, column });
scalar_fields!(MultipleAssignmentTargets<ScalarExpr> { targets } skip { source_positions, source_width });
scalar_fields!(AssignmentPlan { target, value });
impl Retire for crate::ast::ConflictTargetConstraint {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Unqualified => true,
            Self::Named(name) => name.retire_step(positions, scalar),
            Self::Collations(names) => names.retire_step(positions, scalar),
        }
    }
}
scalar_fields!(ConflictPlan {
    predicate,
    expressions,
    action,
    constraint,
    conflict_columns
});
scalar_fields!(ViewCheckPlan { predicate, view });
scalar_fields!(CteCyclePlan {
    mark_value,
    mark_default,
    columns,
    mark_column,
    path_column
});

impl Retire for QueryBlockPlan {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        fields!(positions, scalar; self.windows, self.projections, self.r#where, self.group_by,
            self.grouping_sets, self.having, self.order_by, self.limit, self.offset, self.distinct_on, self.locking)
    }
}
impl Retire for ScalarFrameBound {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Preceding(value) | Self::Following(value) => value.retire_step(positions, scalar),
            Self::UnboundedPreceding | Self::UnboundedFollowing | Self::CurrentRow => true,
        }
    }
}
impl Retire for AssignmentTargets<ScalarExpr> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Single(value) => value.retire_step(positions, scalar),
            Self::Multiple(value) => value.retire_step(positions, scalar),
        }
    }
}
impl Retire for AssignmentStep<ScalarExpr> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Field(name) => name.retire_step(positions, scalar),
            Self::Index(value) => value.retire_step(positions, scalar),
            Self::Slice { lower, upper } => fields!(positions, scalar; lower, upper),
        }
    }
}
impl Retire for ConflictActionPlan {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Nothing => true,
            Self::Update {
                assignments,
                predicate,
            } => fields!(positions, scalar; assignments, predicate),
        }
    }
}
impl Retire for MergeWhenPlan {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => fields!(positions, scalar; condition, assignments),
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => {
                condition.retire_step(positions, scalar)
            }
            Self::InsertNotMatched {
                condition,
                columns,
                overriding: _,
                values,
            } => fields!(positions, scalar; condition, columns, values),
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
impl<T: Retire> Retire for Pending<T> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.0.retire_step(positions, scalar)
    }
}
impl<T: Retire> Drop for Pending<T> {
    fn drop(&mut self) {
        self.0.retire();
    }
}
