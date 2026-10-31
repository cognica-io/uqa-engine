//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Address-stable scalar edits keep read-only selection separate from paired payload/owner replacement.

use super::{children, OwnedScalarExpr, ScalarExpr, ScalarMemory};
use crate::SQLError;
use std::ptr::NonNull;
use uqa_core::memory::ProductionControl;

pub type ScalarNodeTransform<'a> =
    dyn FnMut(NonNull<ScalarExpr>, OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> + 'a;

pub(crate) type Replacement<'a> =
    dyn FnMut(&ScalarExpr) -> Result<Option<OwnedScalarExpr>, SQLError> + 'a;

impl OwnedScalarExpr {
    /// Transform children before their parent, retaining the original address as an opaque selection identity. The callback owns all surviving children and may move them into an admitted wrapper.
    pub fn transform_nodes(
        mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        Self::transform_nodes_in_place(
            &mut self.expression,
            self.memory.as_mut(),
            control,
            transform,
        )?;
        Ok(self)
    }

    /// Internal analysis buffers retain each expression's address until this paired in-place transform finishes. Failure leaves the private owner valid for destruction; callers do not publish it.
    pub(crate) fn transform_retained_nodes(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        Self::transform_nodes_in_place(
            &mut self.expression,
            self.memory.as_mut(),
            control,
            transform,
        )
    }

    pub(crate) fn transform_nodes_in_place(
        expression: &mut ScalarExpr,
        mut memory: Option<&mut ScalarMemory>,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let count = children::count(expression);
        for ordinal in 0..count {
            let index = visit_index(expression, ordinal, count);
            let child = children::get_mut(expression, index).expect("scalar child");
            let owner = memory
                .as_mut()
                .map(|memory| memory.children[index].as_mut().expect("scalar child owner"));
            Self::transform_nodes_in_place(child, owner, control, transform)?;
        }
        let identity = NonNull::from(&*expression);
        let owner = memory.as_mut().map(|owner| {
            let direct = control.empty_reservation().expect("controlled scalar slot");
            let empty = ScalarMemory {
                children: uqa_core::memory::BudgetedVec::new(direct.budget()),
                direct,
            };
            std::mem::replace(*owner, empty)
        });
        let input = Self::from_rewrite_parts(
            std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null)),
            owner,
        );
        let output = transform(identity, input)?;
        output.assert_owner(control);
        control.check()?;
        let (output, owner) = output.into_rewrite_parts();
        *expression = output;
        if let Some(memory) = memory {
            *memory = owner.expect("controlled scalar transformation");
        }
        Ok(())
    }

    /// Replace selected nodes in pre-order. The observer receives no mutable syntax; every returned node carries its own complete constructor owner.
    pub fn replace_nodes(
        mut self,
        control: &ProductionControl<'_>,
        replacement: &mut Replacement<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        Self::replace_nodes_in_place(
            &mut self.expression,
            self.memory.as_mut(),
            control,
            replacement,
        )?;
        Ok(self)
    }

    pub(crate) fn replace_nodes_in_place(
        expression: &mut ScalarExpr,
        mut memory: Option<&mut ScalarMemory>,
        control: &ProductionControl<'_>,
        replacement: &mut Replacement<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(output) = replacement(expression)? {
            output.assert_owner(control);
            let (output, owner) = output.into_rewrite_parts();
            *expression = output;
            if let Some(memory) = &mut memory {
                **memory = owner.expect("replacement has its own scalar constructor");
            }
        }
        let count = children::count(expression);
        for ordinal in 0..count {
            let index = visit_index(expression, ordinal, count);
            let child = children::get_mut(expression, index).expect("scalar child");
            let owner = memory
                .as_mut()
                .map(|memory| memory.children[index].as_mut().expect("scalar child owner"));
            Self::replace_nodes_in_place(child, owner, control, replacement)?;
        }
        control.check()?;
        Ok(())
    }
}

fn visit_index(expression: &ScalarExpr, ordinal: usize, count: usize) -> usize {
    // Read-only/mutable syntax visitation places the window FILTER before its specification; constructor ownership stores it last.
    if let ScalarExpr::WindowCall {
        args,
        filter: Some(_),
        ..
    } = expression
    {
        if ordinal == args.len() {
            return count - 1;
        }
        if ordinal > args.len() {
            return ordinal - 1;
        }
    }
    ordinal
}
