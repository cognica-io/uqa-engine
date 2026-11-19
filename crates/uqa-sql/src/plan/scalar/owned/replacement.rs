//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Address-stable scalar edits keep read-only selection separate from paired payload/owner replacement.

use super::{
    traversal::{Edit, EditIdentity},
    OwnedScalarExpr, ScalarExpr, ScalarMemory,
};
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
        self.transform_retained_nodes(control, transform)?;
        Ok(self)
    }

    /// Internal analysis buffers retain each expression's address until this paired in-place transform finishes. Failure leaves the private owner valid for destruction; callers do not publish it.
    pub(crate) fn transform_retained_nodes(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        crate::catalog::work::inline(self.edit_retained_from(
            control,
            &mut (),
            &mut Transform(transform),
        ))
    }

    pub(crate) async fn edit_retained_from<W: super::super::resources::input::Work, E: Edit<W>>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
        edit: &mut E,
    ) -> Result<(), SQLError>
    where
        W::Error: Into<SQLError>,
    {
        self.assert_owner(control);
        edit_in_place_from(
            &mut self.expression,
            self.memory.as_mut(),
            control,
            work,
            edit,
        )
        .await
    }

    pub(crate) fn transform_nodes_in_place(
        expression: &mut ScalarExpr,
        memory: Option<&mut ScalarMemory>,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        edit_in_place(expression, memory, control, &mut Transform(transform))
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
        memory: Option<&mut ScalarMemory>,
        control: &ProductionControl<'_>,
        replacement: &mut Replacement<'_>,
    ) -> Result<(), SQLError> {
        edit_in_place(expression, memory, control, &mut Replace(replacement))
    }
}

fn edit_in_place<E: Edit<()>>(
    expression: &mut ScalarExpr,
    memory: Option<&mut ScalarMemory>,
    control: &ProductionControl<'_>,
    edit: &mut E,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(edit_in_place_from(
        expression,
        memory,
        control,
        &mut (),
        edit,
    ))
}

async fn edit_in_place_from<W: super::super::resources::input::Work, E: Edit<W>>(
    expression: &mut ScalarExpr,
    mut memory: Option<&mut ScalarMemory>,
    control: &ProductionControl<'_>,
    work: &mut W,
    edit: &mut E,
) -> Result<(), SQLError>
where
    W::Error: Into<SQLError>,
{
    control.check()?;
    let identity = E::identity(expression);
    let selected = edit.select(expression)?;
    let owner = memory.as_mut().map(|owner| {
        std::mem::replace(
            *owner,
            ScalarMemory::new(control.empty_reservation().expect("controlled scalar slot")),
        )
    });
    let input = OwnedScalarExpr::from_rewrite_parts(
        std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null)),
        owner,
    );
    let output = selected
        .unwrap_or(input)
        .edit_from(identity, control, work, edit)
        .await?;
    let (output, owner) = output.into_rewrite_parts();
    *expression = output;
    if let Some(memory) = memory {
        *memory = owner.expect("controlled scalar edit");
    }
    Ok(())
}

struct Transform<'a, 'b>(&'a mut ScalarNodeTransform<'b>);
impl EditIdentity for Transform<'_, '_> {
    type Identity = NonNull<ScalarExpr>;
    const BEFORE_CHILDREN: bool = false;
    fn identity(expression: &ScalarExpr) -> Self::Identity {
        NonNull::from(expression)
    }
}
impl Edit<()> for Transform<'_, '_> {
    fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        _: &ProductionControl<'_>,
        (): &mut (),
    ) -> impl std::future::Future<Output = Result<OwnedScalarExpr, SQLError>> + Send {
        std::future::ready((self.0)(identity, expression))
    }
}

struct Replace<'a, 'b>(&'a mut Replacement<'b>);
impl EditIdentity for Replace<'_, '_> {
    type Identity = ();
    const BEFORE_CHILDREN: bool = true;
    fn identity(_: &ScalarExpr) {}
    fn select(&mut self, expression: &ScalarExpr) -> Result<Option<OwnedScalarExpr>, SQLError> {
        (self.0)(expression)
    }
}
impl Edit<()> for Replace<'_, '_> {}

pub(super) fn visit_index(expression: &ScalarExpr, ordinal: usize, count: usize) -> usize {
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
