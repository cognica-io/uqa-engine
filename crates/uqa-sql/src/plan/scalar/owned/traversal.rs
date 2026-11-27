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

pub(crate) trait EditIdentity<Node = OwnedScalarExpr> {
    type Identity: Copy;
    const BEFORE_CHILDREN: bool;

    fn identity(expression: &ScalarExpr) -> Self::Identity;

    /// Observe a borrowed node in its original slot before moving its payload into the editor.
    fn select(&mut self, _expression: &ScalarExpr) -> Result<Option<Node>, SQLError> {
        Ok(None)
    }

    fn child_count(expression: &ScalarExpr) -> usize {
        children::count(expression)
    }
}

pub(crate) trait Edit<W: Work, Node: Send = OwnedScalarExpr>: EditIdentity<Node> {
    fn apply(
        &mut self,
        _identity: Self::Identity,
        expression: Node,
        _control: &ProductionControl<'_>,
        _work: &mut W,
    ) -> impl Future<Output = Result<Node, SQLError>> + Send {
        std::future::ready(Ok(expression))
    }
}

/// The original allocation owner accompanies each moved node. A published field whose lease is held by its enclosing statement uses the same traversal as a scalar with separate child receipts.
trait Payload: std::ops::Deref<Target = ScalarExpr> + Send {
    fn scalar(&mut self) -> &mut ScalarExpr;
    fn take_child(&mut self, index: usize) -> Self;
    fn restore_child(&mut self, index: usize, value: Self);
    fn validate(&self, control: &ProductionControl<'_>);
}

impl Payload for OwnedScalarExpr {
    fn scalar(&mut self) -> &mut ScalarExpr {
        &mut self.expression
    }
    fn take_child(&mut self, index: usize) -> Self {
        self.take_child(index)
    }
    fn restore_child(&mut self, index: usize, value: Self) {
        self.restore_child(index, value);
    }
    fn validate(&self, control: &ProductionControl<'_>) {
        self.assert_owner(control);
    }
}

type StoredField = crate::plan::retirement::Pending<ScalarExpr>;

impl Payload for StoredField {
    fn scalar(&mut self) -> &mut ScalarExpr {
        self
    }
    fn take_child(&mut self, index: usize) -> Self {
        let slot = children::get_mut(self, index).expect("visited scalar child");
        Self::new(std::mem::replace(slot, ScalarExpr::Star))
    }
    fn restore_child(&mut self, index: usize, value: Self) {
        *children::get_mut(self, index).expect("visited scalar child") = value.take();
    }
    fn validate(&self, _: &ProductionControl<'_>) {}
}

struct Parent<N, I> {
    expression: N,
    identity: I,
    child: usize,
    next: usize,
}

impl OwnedScalarExpr {
    pub(in crate::plan) fn edit_from<'a, W: Work, E: Edit<W>>(
        self,
        identity: E::Identity,
        control: &'a ProductionControl<'a>,
        work: &'a mut W,
        edit: &'a mut E,
    ) -> impl Future<Output = Result<Self, SQLError>> + 'a
    where
        W::Error: Into<SQLError>,
        E::Identity: 'a,
    {
        traverse(self, identity, control, work, edit)
    }
}

/// Work on a privately owned statement field after its original lease has been folded into the enclosing executable. Failed or cancelled admission retires the private prefix through its original scalar owner.
pub(crate) async fn edit_statement_scalar<W: Work, E: Edit<W, StoredField>>(
    value: &mut ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    edit: &mut E,
) -> Result<(), SQLError>
where
    W::Error: Into<SQLError>,
{
    let identity = E::identity(value);
    let original = StoredField::new(std::mem::replace(value, ScalarExpr::Star));
    *value = traverse(original, identity, control, work, edit)
        .await?
        .take();
    Ok(())
}

async fn traverse<N: Payload, W: Work, E: Edit<W, N>>(
    mut value: N,
    mut identity: E::Identity,
    control: &ProductionControl<'_>,
    work: &mut W,
    edit: &mut E,
) -> Result<N, SQLError>
where
    W::Error: Into<SQLError>,
{
    value.validate(control);
    let mut parents = Stack::<_, 4>::new(*control);
    let mut entering = true;
    let mut ordinal = 0;
    loop {
        control.check()?;
        work.checkpoint().await.map_err(Into::into)?;
        control.check()?;
        if entering {
            if E::BEFORE_CHILDREN {
                value = edit.apply(identity, value, control, work).await?;
                value.validate(control);
            }
            entering = false;
        }
        let count = E::child_count(&value);
        if ordinal < count {
            let child = visit_index(&value, ordinal, count);
            parents.push(Parent {
                expression: value,
                identity,
                child,
                next: ordinal + 1,
            })?;
            // Admit the parent before observing its child. The slot remains in the original
            // box or vector until both its analysis identity and any replacement are selected.
            control.check()?;
            let parent = parents.last_mut().expect("admitted scalar parent");
            let slot = children::get_mut(parent.expression.scalar(), child).expect("scalar child");
            identity = E::identity(slot);
            let selected = edit.select(slot)?;
            let original = parent.expression.take_child(child);
            value = selected.unwrap_or(original);
            value.validate(control);
            ordinal = 0;
            entering = true;
        } else {
            if !E::BEFORE_CHILDREN {
                value = edit.apply(identity, value, control, work).await?;
                value.validate(control);
            }
            if let Some(mut parent) = parents.pop() {
                parent.expression.restore_child(parent.child, value);
                value = parent.expression;
                identity = parent.identity;
                ordinal = parent.next;
            } else {
                control.check()?;
                return Ok(value);
            }
        }
    }
}
