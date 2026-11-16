//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary controlled lowering uses the same iterative constructors with immediately ready query callbacks.

use super::{construct_scalar, ScalarQueryInput};
use crate::{
    ast::{Expr, SelectStmt},
    plan::{scalar::QueryLowering, AggregateClassifier, OwnedScalarExpr},
    schema::retention::CatalogRetentionError,
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

struct Queries<'a> {
    arena: &'a mut dyn QueryLowering,
    aggregates: &'a dyn AggregateClassifier,
}
impl<'s> ScalarQueryInput<&'s Expr, &'s SelectStmt, CatalogRetentionError> for Queries<'_> {
    fn parameter(
        &mut self,
        expression: &'s Expr,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Option<usize>, CatalogRetentionError>> + Send {
        ready(
            control
                .check()
                .map(|()| self.arena.parameter(expression))
                .map_err(Into::into),
        )
    }
    fn append(
        &mut self,
        query: &'s SelectStmt,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<usize, CatalogRetentionError>> + Send {
        ready(
            control
                .check()
                .map_err(CatalogRetentionError::from)
                .and_then(|()| self.arena.append_borrowed(query, self.aggregates)),
        )
    }
}

pub(in crate::plan::scalar::lowering) fn lower_inline(
    expression: &Expr,
    aggregates: &dyn AggregateClassifier,
    arena: &mut dyn QueryLowering,
    control: ProductionControl<'_>,
) -> Result<OwnedScalarExpr, CatalogRetentionError> {
    crate::catalog::work::inline(construct_scalar(
        expression,
        &mut Queries { arena, aggregates },
        &control,
        &mut (),
        true,
    ))
}

pub(in crate::plan::scalar::lowering) fn lower_inline_immutable(
    expression: &Expr,
    control: &ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<crate::ScalarExpr>, CatalogRetentionError> {
    crate::catalog::work::inline(construct_scalar(
        expression,
        &mut Queries {
            arena: &mut Vec::new(),
            aggregates: &crate::plan::NoRegisteredAggregates,
        },
        control,
        &mut (),
        false,
    ))?
    .finish(control)
}
