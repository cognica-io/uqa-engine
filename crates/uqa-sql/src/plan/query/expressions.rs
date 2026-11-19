//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local expression roots retain their namespace and expose empty-container progress to suspended consumers.

use super::{ComputePlan, ProjectionPlan, QueryBlockPlan, ScalarExpr, SourcePlan};

#[derive(Clone, Copy)]
pub(in crate::plan) enum ExpressionOrder {
    Initialization,
    Rewrite,
    Visit,
}

macro_rules! query_roots {
    ($method:ident, $iter:ident, $reference:ident, $slice:ident, $expressions:ident, [$($mutable:tt)*]) => {
        impl QueryBlockPlan {
            pub(in crate::plan) fn $method(
                & $($mutable)* self,
                order: ExpressionOrder,
            ) -> impl Iterator<Item = Option<& $($mutable)* ScalarExpr>> {
                let (before, after): (& $($mutable)* [ProjectionPlan], & $($mutable)* [ProjectionPlan]) =
                    if matches!(order, ExpressionOrder::Rewrite) || (matches!(order, ExpressionOrder::Initialization) && matches!(self.compute, ComputePlan::Project)) {
                        (self.projections.$slice(), & $($mutable)* [])
                    } else {
                        (& $($mutable)* [], self.projections.$slice())
                    };
                before.$iter().map(|projection| Some(& $($mutable)* projection.expr))
                    .chain(self.r#where.$reference().map(Some))
                    .chain(after.$iter().map(|projection| Some(& $($mutable)* projection.expr)))
                    .chain(self.group_by.$iter().map(Some))
                    .chain(self.grouping_sets.$iter().flat_map(|set| std::iter::once(None).chain(set.$iter().map(Some))))
                    .chain(self.having.$reference().map(Some))
                    .chain(self.order_by.$iter().map(|order| Some(& $($mutable)* order.expr)))
                    .chain(self.limit.$reference().map(Some))
                    .chain(self.offset.$reference().map(Some))
                    .chain(self.distinct_on.$iter().map(Some))
                    .chain(self.windows.$iter().flat_map(|window| std::iter::once(None).chain(window.spec.$expressions().map(Some))))
            }
        }
    };
}
query_roots!(expression_steps, iter, as_ref, as_slice, expressions, []);
query_roots!(expression_steps_mut, iter_mut, as_mut, as_mut_slice, expressions_mut, [mut]);

macro_rules! source_roots {
    ($method:ident, $iter:ident, $reference:ident, [$($mutable:tt)*]) => {
        impl SourcePlan {
            pub(in crate::plan) fn $method(
                & $($mutable)* self,
            ) -> ([Option<& $($mutable)* Self>; 2], impl Iterator<Item = Option<& $($mutable)* ScalarExpr>>) {
                let (children, predicate, rows, args, functions) = match self {
                    Self::Join { left, right, on, .. } => ([Some(left.$reference()), Some(right.$reference())], on.$reference(), None, None, None),
                    Self::Values { rows, .. } => ([None, None], None, Some(rows), None, None),
                    Self::Function { args, .. } => ([None, None], None, None, Some(args), None),
                    Self::FunctionGroup { functions, .. } => ([None, None], None, None, None, Some(functions)),
                    Self::Table { .. } | Self::Subquery { .. } | Self::RetainedRows(_) => ([None, None], None, None, None, None),
                };
                let expressions = predicate.into_iter().map(Some)
                    .chain(rows.into_iter().flat_map(|rows| rows.$iter().flat_map(|row| std::iter::once(None).chain(row.$iter().map(Some)))))
                    .chain(args.into_iter().flat_map(|args| args.$iter().map(Some)))
                    .chain(functions.into_iter().flat_map(|functions| functions.$iter().flat_map(|function| std::iter::once(None).chain(function.args.$iter().map(Some)))));
                (children, expressions)
            }
        }
    };
}
source_roots!(expression_inputs, iter, as_ref, []);
source_roots!(expression_inputs_mut, iter_mut, as_mut, [mut]);
