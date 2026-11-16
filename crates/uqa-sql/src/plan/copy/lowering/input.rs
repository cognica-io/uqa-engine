//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar callbacks share the ordinary query arena with retained plan constructors.

use super::{Copying, QueryArena, Result, RewriteField};
use crate::{
    ast::Expr,
    plan::{AggregateClassifier, OwnedScalarExpr},
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

pub(super) trait ScalarInput<Expression> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send;
    fn scalar(
        &mut self,
        expression: Expression,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send;
}

pub(super) struct InlineScalarInput<'a, 'c> {
    pub(super) aggregates: &'a dyn AggregateClassifier,
    pub(super) arena: &'a mut QueryArena<'c>,
}

impl<'s> ScalarInput<&'s crate::ast::Expr> for InlineScalarInput<'_, '_> {
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

impl Copying<'_> {
    /// Admit the parent record before a field constructor can suspend with its own allocation owner.
    pub(super) fn begin_input_field(&mut self) -> Result<Self> {
        self.control.check()?;
        self.fields.as_mut().expect("retained fields").reserve(1)?;
        let mut child = Self::for_rewrite(self.control).expect("retained field allowance");
        child.parameters = self.parameters;
        Ok(child)
    }

    /// Transfer a completed field's constructor records into the already admitted parent slot.
    pub(super) fn finish_input_field(&mut self, name: &'static str, child: Self) {
        self.fields
            .as_mut()
            .expect("retained fields")
            .push(RewriteField {
                name,
                memory: Some(child.into_rewrite_memory()),
            })
            .expect("field record admitted before construction");
    }
}
