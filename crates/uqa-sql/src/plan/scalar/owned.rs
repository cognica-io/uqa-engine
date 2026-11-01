//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable scalar ownership follows constructor provenance instead of estimating a rewritten tree.

use super::{resources::Result, ScalarExpr};
use crate::schema::retention::CatalogRetentionError;
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl},
    Value,
};

mod call;
mod children;
mod composite;
mod composites;
mod construction;
mod constructors;
mod edit;
mod generated;
mod iteration;
mod literals;
mod metadata;
mod query_ids;
mod references;
mod replacement;
mod selection;
pub(in crate::plan) mod windows;
pub use iteration::OwnedScalarChildren;
pub use replacement::ScalarNodeTransform;

/// A scalar expression whose child allocations can be transferred or discarded independently. The expression is read-only outside this owner's consuming operations; replacing raw IR through a mutable dereference would separate allocations from their reservations.
#[derive(Debug)]
pub struct OwnedScalarExpr {
    // Drop the complete expression before returning any of its allowances.
    pub(super) expression: ScalarExpr,
    pub(super) memory: Option<ScalarMemory>,
}

/// Direct allocations include this node's names, types, values, child boxes and child-vector buffers. Each child's payload has its own owner; the metadata vector also admits its actual capacity before allocation.
#[derive(Debug)]
pub(crate) struct ScalarMemory {
    // The direct lease includes this metadata buffer; destroy it before releasing the lease.
    pub(super) children: Vec<Option<Self>>,
    pub(super) direct: MemoryReservation,
}

impl ScalarMemory {
    pub(super) fn from_parts(
        mut direct: MemoryReservation,
        children: BudgetedVec<Option<Self>>,
    ) -> Self {
        let (children, memory) = children.into_parts();
        direct.absorb(memory);
        Self { children, direct }
    }

    fn into_parts(self) -> (MemoryReservation, BudgetedVec<Option<Self>>) {
        let Self {
            children,
            mut direct,
        } = self;
        let memory = direct.split(children.capacity() * size_of::<Option<Self>>());
        (direct, BudgetedVec::from_parts(children, memory))
    }

    fn child_buffer(&mut self) -> ScalarChildren<'_> {
        let children = std::mem::take(&mut self.children);
        let memory = self
            .direct
            .split(children.capacity() * size_of::<Option<Self>>());
        ScalarChildren {
            parent: self,
            buffer: Some(BudgetedVec::from_parts(children, memory)),
        }
    }

    pub(super) fn reserve_children(&mut self, count: usize) -> Result<()> {
        self.child_buffer()
            .buffer
            .as_mut()
            .expect("child buffer")
            .reserve(count)?;
        Ok(())
    }

    pub(super) fn push_child(&mut self, child: Option<Self>) -> Result<()> {
        self.child_buffer()
            .buffer
            .as_mut()
            .expect("child buffer")
            .push(child)?;
        Ok(())
    }

    pub(crate) fn reserved_bytes(&self) -> usize {
        self.direct.bytes()
            + self
                .children
                .iter()
                .flatten()
                .map(Self::reserved_bytes)
                .sum::<usize>()
    }

    pub(crate) fn into_reservation(self) -> MemoryReservation {
        let (mut direct, mut children) = self.into_parts();
        while let Some(child) = children.pop() {
            direct.absorb(child.expect("owned scalar child").into_reservation());
        }
        // Ownership metadata is temporary, not part of the immutable scalar allocation.
        drop(children);
        direct
    }
}

/// Reattach metadata on success, rejection and unwinding before the parent can release its payloads.
struct ScalarChildren<'a> {
    parent: &'a mut ScalarMemory,
    buffer: Option<BudgetedVec<Option<ScalarMemory>>>,
}

impl Drop for ScalarChildren<'_> {
    fn drop(&mut self) {
        let (children, memory) = self.buffer.take().expect("child buffer").into_parts();
        self.parent.children = children;
        self.parent.direct.absorb(memory);
    }
}

impl OwnedScalarExpr {
    pub(crate) fn into_rewrite_parts(self) -> (ScalarExpr, Option<ScalarMemory>) {
        (self.expression, self.memory)
    }

    pub(crate) fn from_rewrite_parts(expression: ScalarExpr, memory: Option<ScalarMemory>) -> Self {
        Self { expression, memory }
    }

    /// Move an ordinary scalar without claiming that its existing allocations were admitted. Controlled construction uses `ScalarExpr::copy_for_rewrite` instead.
    pub fn uncontrolled(expression: ScalarExpr) -> Self {
        Self {
            expression,
            memory: None,
        }
    }

    /// Construct a scalar leaf from a value whose producer already owns its payload allowance.
    pub fn literal(value: Produced<Value>, control: &ProductionControl<'_>) -> Result<Self> {
        let (value, memory) = value.into_parts();
        let output = Self {
            expression: ScalarExpr::Literal(value),
            memory: memory.map(|direct| ScalarMemory {
                children: Vec::new(),
                direct,
            }),
        };
        output.assert_owner(control);
        control.check()?;
        Ok(output)
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory.as_ref().map_or(0, ScalarMemory::reserved_bytes)
    }

    /// Rewrite a complete scalar owner. The callback may replace or select the root, but both its input and output must retain this production scope. Failure destroys all private inputs before returning.
    pub fn rewrite<E>(
        self,
        control: &ProductionControl<'_>,
        rewrite: impl FnOnce(Self) -> std::result::Result<Self, E>,
    ) -> std::result::Result<Self, E>
    where
        E: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        let output = rewrite(self)?;
        output.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        Ok(output)
    }

    /// Number of immediate scalar children, in copy order: call arguments, ordering keys and filter; window arguments, partition keys, ordering keys, frame bounds and filter; CASE base, condition/result pairs and ELSE.
    pub fn child_count(&self) -> usize {
        children::count(&self.expression)
    }

    /// Move one child out and destroy the parent and its other children. The child's former box or vector slot belongs to the discarded parent, so only the selected child's own allocations survive.
    pub fn select_child(mut self, index: usize) -> Self {
        let child = self.take_child(index);
        drop(self);
        child
    }

    fn take_child(&mut self, index: usize) -> Self {
        Self {
            expression: std::mem::replace(
                children::get_mut(&mut self.expression, index).expect("scalar child index"),
                ScalarExpr::Literal(Value::Null),
            ),
            memory: self
                .memory
                .as_mut()
                .map(|memory| memory.children[index].take().expect("owned scalar child")),
        }
    }

    /// Rewrite immediate children without reallocating the parent's boxes or vectors. Each callback consumes one complete owner and must return an owner from the same production scope. Error or unwind destroys the private rewrite, including unvisited children; no partly rewritten tree escapes.
    pub fn map_children<E>(
        mut self,
        control: &ProductionControl<'_>,
        mut map: impl FnMut(usize, Self) -> std::result::Result<Self, E>,
    ) -> std::result::Result<Self, E>
    where
        E: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        for ordinal in 0..self.child_count() {
            control.check().map_err(CatalogRetentionError::from)?;
            let child = self.take_child(ordinal);
            let replacement = map(ordinal, child)?;
            replacement.assert_owner(control);
            control.check().map_err(CatalogRetentionError::from)?;
            let Self { expression, memory } = replacement;
            *children::get_mut(&mut self.expression, ordinal).expect("scalar child index") =
                expression;
            if let Some(parent) = &mut self.memory {
                parent.children[ordinal] = memory;
            }
        }
        control.check().map_err(CatalogRetentionError::from)?;
        Ok(self)
    }

    /// Rewrite one immediate child while retaining all parent containers and sibling owners.
    pub fn map_child<E>(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(Self) -> std::result::Result<Self, E>,
    ) -> std::result::Result<Self, E>
    where
        E: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        let replacement = map(self.take_child(index))?;
        replacement.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        let Self { expression, memory } = replacement;
        *children::get_mut(&mut self.expression, index).expect("scalar child index") = expression;
        if let Some(parent) = &mut self.memory {
            parent.children[index] = memory;
        }
        Ok(self)
    }

    /// Freeze a completed rewrite. Surviving IR reservations become one immutable lease, while the temporary ownership metadata is destroyed and its capacity released.
    pub fn finish(self, control: &ProductionControl<'_>) -> Result<Produced<ScalarExpr>> {
        self.assert_owner(control);
        control.check()?;
        let Self { expression, memory } = self;
        let memory = memory.map(ScalarMemory::into_reservation);
        Ok(control.finish(expression, memory)?)
    }

    fn assert_owner(&self, control: &ProductionControl<'_>) {
        match (&self.memory, control.budget()) {
            (Some(memory), Some(budget)) => assert!(
                memory.direct.budget().shares_allowance(budget),
                "different scalar production allowance"
            ),
            (None, None) => {}
            _ => panic!("controlled and ordinary scalar ownership must not be mixed"),
        }
    }
}

impl std::ops::Deref for OwnedScalarExpr {
    type Target = ScalarExpr;

    fn deref(&self) -> &Self::Target {
        &self.expression
    }
}

#[cfg(test)]
mod tests;
