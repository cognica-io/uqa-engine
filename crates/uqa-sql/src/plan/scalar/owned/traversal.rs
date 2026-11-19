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

pub(crate) trait EditIdentity {
    type Identity: Copy;
    const BEFORE_CHILDREN: bool;

    fn identity(expression: &ScalarExpr) -> Self::Identity;

    /// Observe a borrowed node in its original slot before moving its payload into the editor.
    fn select(&mut self, _expression: &ScalarExpr) -> Result<Option<OwnedScalarExpr>, SQLError> {
        Ok(None)
    }

    fn child_count(expression: &ScalarExpr) -> usize {
        children::count(expression)
    }
}

pub(crate) trait Edit<W: Work>: EditIdentity {
    fn apply(
        &mut self,
        _identity: Self::Identity,
        expression: OwnedScalarExpr,
        _control: &ProductionControl<'_>,
        _work: &mut W,
    ) -> impl Future<Output = Result<OwnedScalarExpr, SQLError>> + Send {
        std::future::ready(Ok(expression))
    }
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
            let count = E::child_count(&self);
            if ordinal < count {
                let child = visit_index(&self, ordinal, count);
                parents.push(Parent {
                    expression: self,
                    identity,
                    child,
                    next: ordinal + 1,
                })?;
                // Admit the parent before observing its child. The slot remains in the original
                // box or vector until both its analysis identity and any replacement are selected.
                control.check()?;
                let parent = parents.last_mut().expect("admitted scalar parent");
                let slot = children::get_mut(&mut parent.expression.expression, child)
                    .expect("scalar child");
                identity = E::identity(slot);
                let selected = edit.select(slot)?;
                let original = parent.expression.take_child(child);
                self = selected.unwrap_or(original);
                self.assert_owner(control);
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
