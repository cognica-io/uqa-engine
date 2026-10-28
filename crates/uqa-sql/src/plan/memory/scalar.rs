//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

use crate::ir::{ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec};

impl ObserveAllocations for ScalarExpr {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive observation follows every scalar allocation without evaluation"
    )]
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Star => {}
            Self::QualifiedStar(value0) => {
                observer.child(value0)?;
            }
            Self::Default => {}
            Self::Column(value0) => {
                observer.child(value0)?;
            }
            Self::Position(_) => {}
            Self::InternalColumn(_) => {}
            Self::QualifiedColumn { qualifier, column } => {
                observer.child(qualifier)?;
                observer.child(column)?;
            }
            Self::Literal(value0) => {
                observer.child(value0)?;
            }
            Self::TypedLiteral {
                value,
                ty,
                collation_oid: _,
                composite_source,
                bound_type,
                parameter_index: _,
            } => {
                observer.child(value)?;
                observer.child(ty)?;
                observer.child(composite_source)?;
                observer.child(bound_type)?;
            }
            Self::Collated {
                expr,
                ty,
                collation: _,
            } => {
                observer.child(expr)?;
                observer.child(ty)?;
            }
            Self::Param(_) => {}
            Self::Func {
                name,
                binding,
                args,
                distinct: _,
                order_by,
                order_syntax: _,
                filter,
            } => {
                observer.child(name)?;
                observer.child(binding)?;
                observer.child(args)?;
                observer.child(order_by)?;
                observer.child(filter)?;
            }
            Self::Array(value0) => {
                observer.child(value0)?;
            }
            Self::Row(value0) => {
                observer.child(value0)?;
            }
            Self::CompositeRow {
                items,
                binding,
                bound_type,
            } => {
                observer.child(items)?;
                observer.child(binding)?;
                observer.child(bound_type)?;
            }
            Self::Binary { op: _, lhs, rhs } => {
                observer.child(lhs)?;
                observer.child(rhs)?;
            }
            Self::UnaryMinus(value0) => {
                observer.child(value0)?;
            }
            Self::Not(value0) => {
                observer.child(value0)?;
            }
            Self::And(value0) => {
                observer.child(value0)?;
            }
            Self::Or(value0) => {
                observer.child(value0)?;
            }
            Self::IsNull { expr, negated: _ } => {
                observer.child(expr)?;
            }
            Self::Between { expr, low, high } => {
                observer.child(expr)?;
                observer.child(low)?;
                observer.child(high)?;
            }
            Self::InList {
                expr,
                list,
                negated: _,
            } => {
                observer.child(expr)?;
                observer.child(list)?;
            }
            Self::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers: _,
            } => {
                observer.child(name)?;
                observer.child(args)?;
                observer.child(spec)?;
                observer.child(filter)?;
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                observer.child(base)?;
                observer.child(when)?;
                observer.child(else_branch)?;
            }
            Self::Cast {
                implicit: _,
                expr,
                ty,
            } => {
                observer.child(expr)?;
                observer.child(ty)?;
            }
            Self::ScalarSubquery(_) => {}
            Self::Exists {
                subquery: _,
                negated: _,
            } => {}
            Self::InSubquery {
                expr,
                subquery: _,
                negated: _,
            } => {
                observer.child(expr)?;
            }
            Self::RetainedLiteral(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for ScalarOrder {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            expr,
            descending: _,
            nulls: _,
        } = self;
        observer.child(expr)?;
        Ok(())
    }
}

impl ObserveAllocations for ScalarWindowSpec {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            definition: _,
            partition_by,
            order_by,
            frame,
        } = self;
        observer.child(partition_by)?;
        observer.child(order_by)?;
        observer.child(frame)?;
        Ok(())
    }
}

impl ObserveAllocations for ScalarWindowFrame {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            mode: _,
            start,
            end,
            between: _,
            exclusion: _,
        } = self;
        observer.child(start)?;
        observer.child(end)?;
        Ok(())
    }
}

impl ObserveAllocations for ScalarFrameBound {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::UnboundedPreceding => {}
            Self::UnboundedFollowing => {}
            Self::CurrentRow => {}
            Self::Preceding(value0) => {
                observer.child(value0)?;
            }
            Self::Following(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}
