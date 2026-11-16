//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar callbacks share the ordinary query arena with retained plan constructors.

use super::{QueryArena, Result};
use crate::{
    ast::Expr,
    plan::{AggregateClassifier, OwnedScalarExpr},
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

pub(super) trait ScalarInput<'s> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send;
    fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send;
}

pub(super) struct InlineScalarInput<'a, 'c> {
    pub(super) aggregates: &'a dyn AggregateClassifier,
    pub(super) arena: &'a mut QueryArena<'c>,
}

impl<'s> ScalarInput<'s> for InlineScalarInput<'_, '_> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send {
        ready(Ok(()))
    }

    fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send {
        ready(
            crate::plan::scalar::lower_scalar_with_arena(
                expression,
                self.aggregates,
                self.arena,
                control,
            )
            .map_err(Into::into),
        )
    }
}
