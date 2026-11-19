//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar destinations own admitted children throughout reconstruction.

use super::{
    expressions::{boxed, payload},
    invalid, Fields, Input,
};
use crate::{ir::ScalarExpr, SQLError};
use std::io::Read;
use uqa_core::{
    image_io::ImageDecoder,
    memory::{MemoryBudget, MemoryReservation},
    CancellationToken,
};

mod children;
mod continuation;
use continuation::ScalarInput;
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
        let mut owner = ScalarInput::new(self.binary.memory);
        while !owner.complete() {
            self.fields(async |mut input| owner.advance(&mut input).await)?;
        }
        owner.finish()
    }
}

impl Fields<'_> {
    pub(super) async fn native_scalar(&mut self) -> Result<CapturedScalar, SQLError> {
        let mut owner = ScalarInput::new(self.binary.memory);
        while !owner.complete() {
            self.binary.checkpoint().await?;
            owner.advance(self).await?;
        }
        owner.finish()
    }

    async fn native_scalar_slots(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<ScalarExpr>, SQLError> {
        self.slots(count, memory, || ScalarExpr::Star).await
    }

    async fn scalar_image(&mut self) -> Result<CapturedScalar, SQLError> {
        if self.header(super::super::expressions::SCALAR_MAGIC).await? != 1 {
            return Err(invalid("native scalar count"));
        }
        let value = self.native_scalar().await?;
        self.binary.end().await?;
        Ok(value)
    }
}

impl ScalarExpr {
    /// Retain native scalar fields, query/window addresses and original payload ownership across independent byte and continuation-poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<CapturedScalar, SQLError>, SQLError> {
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                Fields {
                    binary: reader.input(),
                    retained_children: &mut 0,
                }
                .scalar_image()
                .await
            },
        )?)
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
