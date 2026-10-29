//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call binding borrows metadata and edits independently owned scalar children.

use crate::{ast::FunctionBinding, plan::OwnedScalarExpr, SQLError, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};

/// A function-call view of SQL's mutable scalar owner. All observations borrow the original IR; edits transfer its constructor reservations with the corresponding fields.
#[derive(Debug)]
pub struct OwnedBindingCall(OwnedScalarExpr);

impl OwnedBindingCall {
    pub fn new(
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        expression
            .rewrite(control, |expression| {
                if !matches!(&*expression, ScalarExpr::Func { .. }) {
                    return Err(SQLError::Internal(
                        "call binding requires a function expression".into(),
                    ));
                }
                Ok(expression)
            })
            .map(Self)
    }

    pub fn into_expression(self) -> OwnedScalarExpr {
        self.0
    }

    pub fn name(&self) -> &str {
        let ScalarExpr::Func { name, .. } = &*self.0 else {
            unreachable!()
        };
        name
    }

    pub fn binding(&self) -> Option<&FunctionBinding> {
        let ScalarExpr::Func { binding, .. } = &*self.0 else {
            unreachable!()
        };
        binding.as_ref()
    }

    pub fn arguments(&self) -> &[ScalarExpr] {
        let ScalarExpr::Func { args, .. } = &*self.0 else {
            unreachable!()
        };
        args
    }

    pub fn order_by(&self) -> &[crate::ScalarOrder] {
        let ScalarExpr::Func { order_by, .. } = &*self.0 else {
            unreachable!()
        };
        order_by
    }

    pub fn filter(&self) -> Option<&ScalarExpr> {
        let ScalarExpr::Func { filter, .. } = &*self.0 else {
            unreachable!()
        };
        filter.as_deref()
    }

    pub fn is_distinct(&self) -> bool {
        let ScalarExpr::Func { distinct, .. } = &*self.0 else {
            unreachable!()
        };
        *distinct
    }

    pub fn map_argument(
        self,
        index: usize,
        control: &ProductionControl<'_>,
        rewrite: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        assert!(index < self.arguments().len(), "call argument index");
        Ok(Self(self.0.map_child(index, control, rewrite)?))
    }

    /// Move a supplied argument into an independent owner, leaving an inline NULL in its original slot until the call publishes its replacement argument vector.
    pub fn take_argument(
        self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, OwnedScalarExpr), SQLError> {
        assert!(index < self.arguments().len(), "call argument index");
        let replacement = OwnedScalarExpr::literal(
            control.finish(uqa_core::Value::Null, control.empty_reservation())?,
            control,
        )?;
        let (parent, argument) = self.0.replace_child(index, replacement, control)?;
        Ok((Self(parent), argument))
    }

    pub fn swap_arguments(
        self,
        left: usize,
        right: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        assert!(
            left < self.arguments().len() && right < self.arguments().len(),
            "call argument index"
        );
        Ok(Self(self.0.swap_children(left, right, control)?))
    }

    pub fn map_arguments(
        mut self,
        control: &ProductionControl<'_>,
        mut rewrite: impl FnMut(usize, OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        for index in 0..self.arguments().len() {
            self = self.map_argument(index, control, |argument| rewrite(index, argument))?;
        }
        Ok(self)
    }

    pub fn map_order(
        self,
        index: usize,
        control: &ProductionControl<'_>,
        rewrite: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        assert!(index < self.order_by().len(), "call ordering index");
        let index = self.arguments().len() + index;
        Ok(Self(self.0.map_child(index, control, rewrite)?))
    }

    pub fn map_filter(
        self,
        control: &ProductionControl<'_>,
        rewrite: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        if self.filter().is_none() {
            return Ok(self);
        }
        let index = self.arguments().len() + self.order_by().len();
        Ok(Self(self.0.map_child(index, control, rewrite)?))
    }

    pub fn replace_binding(
        self,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(self.0.replace_call_binding(binding, control)?))
    }

    /// Install the selected binding while preserving a previously selected input collation when the replacement does not choose one.
    pub fn rebind(
        self,
        binding: Produced<FunctionBinding>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let input_collation = self.binding().and_then(|binding| binding.input_collation);
        let (mut binding, memory) = binding.into_parts();
        if binding.input_collation.is_none() {
            binding.input_collation = input_collation;
        }
        self.replace_binding(Some(control.finish(binding, memory)?), control)
    }

    pub fn set_dispatch(
        self,
        dispatch: Option<crate::ast::FunctionDispatch>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(self.0.set_call_dispatch(dispatch, control)?))
    }

    pub fn replace_metadata(
        self,
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(self.0.replace_call_metadata(name, binding, control)?))
    }

    pub fn take_arguments(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, OwnedScalarExpr), SQLError> {
        let (parent, arguments) = self.0.take_call_arguments(control)?;
        Ok((Self(parent), arguments))
    }

    pub fn replace_arguments(
        self,
        arguments: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(self.0.replace_call_arguments(arguments, control)?))
    }
}

impl std::ops::Deref for OwnedBindingCall {
    type Target = ScalarExpr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
