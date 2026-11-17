//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar edits share admitted parent frames, child order and retirement across inline and suspended work.

use super::{children, replacement::visit_index, OwnedScalarExpr, ScalarExpr};
use crate::{construction::Stack, plan::scalar::resources::input::Work, SQLError};
use std::future::Future;
use uqa_core::memory::ProductionControl;

pub(in crate::plan) trait EditIdentity {
    type Identity: Copy;
    const BEFORE_CHILDREN: bool;

    fn identity(expression: &ScalarExpr) -> Self::Identity;
}

pub(in crate::plan) trait Edit<W: Work>: EditIdentity {
    fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<OwnedScalarExpr, SQLError>> + Send;
}

struct Parent<I> {
    expression: OwnedScalarExpr,
    identity: I,
    child: usize,
    next: usize,
}

impl OwnedScalarExpr {
    pub(in crate::plan) async fn edit_from<W: Work, E: Edit<W>>(
        mut self,
        mut identity: E::Identity,
        control: &ProductionControl<'_>,
        work: &mut W,
        edit: &mut E,
    ) -> Result<Self, SQLError>
    where
        W::Error: Into<SQLError>,
    {
        self.assert_owner(control);
        let mut parents = Stack::<_, 4>::new(*control);
        let mut entering = true;
        let mut ordinal = 0;
        loop {
            control.check()?;
            work.checkpoint().await.map_err(Into::into)?;
            control.check()?;
            if entering {
                if E::BEFORE_CHILDREN {
                    self = edit.apply(identity, self, control, work).await?;
                    self.assert_owner(control);
                }
                entering = false;
            }
            let count = self.child_count();
            if ordinal < count {
                let child = visit_index(&self, ordinal, count);
                // Child slots remain in the original parent boxes/vectors; capture their analysis identity before moving the payload.
                let child_identity = E::identity(
                    children::get_mut(&mut self.expression, child).expect("scalar child"),
                );
                let value = self.take_child(child);
                parents.push(Parent {
                    expression: self,
                    identity,
                    child,
                    next: ordinal + 1,
                })?;
                self = value;
                identity = child_identity;
                ordinal = 0;
                entering = true;
            } else {
                if !E::BEFORE_CHILDREN {
                    self = edit.apply(identity, self, control, work).await?;
                    self.assert_owner(control);
                }
                if let Some(mut parent) = parents.pop() {
                    parent.expression.restore_child(parent.child, self);
                    self = parent.expression;
                    identity = parent.identity;
                    ordinal = parent.next;
                } else {
                    control.check()?;
                    return Ok(self);
                }
            }
        }
    }
}
