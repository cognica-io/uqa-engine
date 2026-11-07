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
    _memory: MemoryReservation,
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
        let mut frame = Frame {
            value: self.native_scalar_node(&mut memory)?,
            cursor: 0,
        };
        loop {
            self.binary.cancellation.check()?;
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
                    _memory: memory,
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
