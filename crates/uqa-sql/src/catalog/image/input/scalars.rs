//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar destinations own admitted children throughout reconstruction.

use super::{
    expressions::{boxed, payload},
    invalid, Input,
};
use crate::{ir::ScalarExpr, SQLError};
use std::io::Read;
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

mod children;
mod nodes;
#[cfg(test)]
mod tests;
mod windows;

/// An immutable native scalar and the allowance for its reconstructed payloads. Retirement belongs to this owner and uses the existing tree for continuation storage, including after decoding failure.
#[derive(Debug)]
pub struct CapturedScalar {
    expression: ScalarExpr,
    memory: MemoryReservation,
    references: References,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct References {
    pub query: Option<usize>,
    pub window: Option<usize>,
}

impl References {
    fn observe(&mut self, value: &ScalarExpr) {
        match value {
            ScalarExpr::ScalarSubquery(query)
            | ScalarExpr::Exists {
                subquery: query, ..
            }
            | ScalarExpr::InSubquery {
                subquery: query, ..
            } => self.query = self.query.max(Some(*query)),
            ScalarExpr::WindowCall { spec, .. } => self.window = self.window.max(spec.definition),
            _ => {}
        }
    }

    pub(super) fn absorb(&mut self, other: Self) {
        self.query = self.query.max(other.query);
        self.window = self.window.max(other.window);
    }
}

impl CapturedScalar {
    /// Only the enclosing native image constructor may transfer this tree, its lease and its scope references into an already owned parent slot.
    pub(super) fn into_native_parts(mut self) -> (ScalarExpr, MemoryReservation, References) {
        let empty = self.memory.budget().empty_reservation();
        (
            std::mem::replace(&mut self.expression, ScalarExpr::Star),
            std::mem::replace(&mut self.memory, empty),
            self.references,
        )
    }
}

impl std::ops::Deref for CapturedScalar {
    type Target = ScalarExpr;
    fn deref(&self) -> &Self::Target {
        &self.expression
    }
}

impl Drop for CapturedScalar {
    fn drop(&mut self) {
        crate::ir::ownership::retire(&mut self.expression);
    }
}

struct Frame {
    value: ScalarExpr,
    cursor: usize,
}

impl Drop for Frame {
    fn drop(&mut self) {
        crate::ir::ownership::retire(&mut self.value);
    }
}

impl Input<'_> {
    pub(super) fn native_scalar(&mut self) -> Result<CapturedScalar, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut parents = BudgetedVec::new(self.binary.memory);
        let mut references = References::default();
        let mut frame = Frame {
            value: self.native_scalar_node(&mut memory)?,
            cursor: 0,
        };
        loop {
            self.binary.cancellation.check()?;
            references.observe(&frame.value);
            if frame.prepare_child(self)? {
                parents.push(frame)?;
                frame = Frame {
                    value: self.native_scalar_node(&mut memory)?,
                    cursor: 0,
                };
            } else if let Some(mut parent) = parents.pop() {
                *parent.child().expect("suspended native scalar child") =
                    std::mem::replace(&mut frame.value, ScalarExpr::Star);
                parent.cursor += 1;
                frame = parent;
            } else {
                return Ok(CapturedScalar {
                    expression: std::mem::replace(&mut frame.value, ScalarExpr::Star),
                    memory,
                    references,
                });
            }
        }
    }

    fn native_scalar_slots(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<ScalarExpr>, SQLError> {
        self.items(count, memory, |_, _| Ok(ScalarExpr::Star))
    }
}

pub(in super::super) fn scalar_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<CapturedScalar, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(super::super::expressions::SCALAR_MAGIC)? != 1 {
        return Err(invalid("native scalar count"));
    }
    let value = input.native_scalar()?;
    input.binary.end()?;
    Ok(value)
}
