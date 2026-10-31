//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

use crate::plan::{
    CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan, ExpressionPlan, OrderPlan, ProjectionPlan,
    QueryBlockPlan, QueryPlan, RelationalPlan, UnifiedPlan,
};

impl ObserveAllocations for UnifiedPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Query(value0) => {
                observer.child(value0)?;
            }
            Self::Command(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for QueryPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            analyzed_output: _,
            analyzed_source: _,
            analyzed_grouping,
            relations_bound: _,
            relation_privilege_subject,
            ctes,
            root,
        } = self;
        observer.child(analyzed_grouping)?;
        observer.child(relation_privilege_subject)?;
        observer.child(ctes)?;
        observer.child(root)?;
        Ok(())
    }
}

impl ObserveAllocations for CtePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            name,
            columns,
            recursive: _,
            materialization: _,
            search,
            cycle,
            body,
        } = self;
        observer.child(name)?;
        observer.child(columns)?;
        observer.child(search)?;
        observer.child(cycle)?;
        observer.child(body)?;
        Ok(())
    }
}

impl ObserveAllocations for CtePlanBody {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Query(value0) => {
                observer.child(value0)?;
            }
            Self::Command(value0) => {
                observer.child(value0)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for CteSearchPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            columns,
            breadth_first: _,
            sequence_column,
        } = self;
        observer.child(columns)?;
        observer.child(sequence_column)?;
        Ok(())
    }
}

impl ObserveAllocations for CteCyclePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            columns,
            mark_column,
            mark_value,
            mark_default,
            path_column,
        } = self;
        observer.child(columns)?;
        observer.child(mark_column)?;
        observer.child(mark_value)?;
        observer.child(mark_default)?;
        observer.child(path_column)?;
        Ok(())
    }
}

impl ObserveAllocations for RelationalPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::QueryBlock(value0) => {
                observer.child(value0)?;
            }
            Self::SetOp {
                kind: _,
                all: _,
                left,
                right,
                order_by,
                limit,
                with_ties: _,
                offset,
                subqueries,
            } => {
                observer.child(left)?;
                observer.child(right)?;
                observer.child(order_by)?;
                observer.child(limit)?;
                observer.child(offset)?;
                observer.child(subqueries)?;
            }
            Self::Values { rows, subqueries } => {
                observer.child(rows)?;
                observer.child(subqueries)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for QueryBlockPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            deferred_outer_rewrites,
            windows,
            projections,
            privilege_columns,
            from,
            r#where,
            compute: _,
            group_by,
            grouping_sets,
            group_distinct: _,
            having,
            order_by,
            limit,
            with_ties: _,
            offset,
            distinct: _,
            distinct_on,
            subqueries,
            access: _,
            locking,
        } = self;
        observer.child(deferred_outer_rewrites)?;
        observer.child(windows)?;
        observer.child(projections)?;
        observer.child(privilege_columns)?;
        observer.child(from)?;
        observer.child(r#where)?;
        observer.child(group_by)?;
        observer.child(grouping_sets)?;
        observer.child(having)?;
        observer.child(order_by)?;
        observer.child(limit)?;
        observer.child(offset)?;
        observer.child(distinct_on)?;
        observer.child(subqueries)?;
        observer.child(locking)?;
        Ok(())
    }
}

impl ObserveAllocations for ProjectionPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { expr, alias } = self;
        observer.child(expr)?;
        observer.child(alias)?;
        Ok(())
    }
}

impl ObserveAllocations for OrderPlan {
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

impl ObserveAllocations for ExpressionPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { scalar, subqueries } = self;
        observer.child(scalar)?;
        observer.child(subqueries)?;
        Ok(())
    }
}
