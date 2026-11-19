//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted native scalar construction keeps unfinished parents in an explicit owner.

use super::{invalid, Fields, Input};
use crate::{ast::Expr, SQLError};
use std::io::Read;
use uqa_core::{
    image_io::{decode_image, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken, Value,
};

mod children;
mod metadata;
mod nodes;
pub(super) use children::expression_child;
#[cfg(test)]
mod tests;

struct Frame {
    value: Expr,
    cursor: usize,
}

pub(super) fn payload<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, reservation) = value.into_parts();
    memory.absorb(reservation);
    value
}

pub(super) fn boxed<T>(value: T, memory: &mut MemoryReservation) -> Result<Box<T>, SQLError> {
    memory.grow(size_of::<T>())?;
    Ok(Box::new(value))
}

impl Fields<'_> {
    pub(super) async fn expression(&mut self) -> Result<Budgeted<Expr>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut parents = BudgetedVec::new(self.binary.memory);
        let mut frame = Frame {
            value: Expr::Star,
            cursor: 0,
        };
        self.expression_node(&mut memory, &mut frame.value).await?;
        loop {
            self.binary.checkpoint().await?;
            if frame.prepare_child(self).await? {
                parents.push(frame)?;
                frame = Frame {
                    value: Expr::Star,
                    cursor: 0,
                };
                self.expression_node(&mut memory, &mut frame.value).await?;
            } else if let Some(mut parent) = parents.pop() {
                *parent.child().expect("suspended scalar child") = frame.value;
                parent.cursor += 1;
                frame = parent;
            } else {
                return Ok(Budgeted::new(frame.value, memory));
            }
        }
    }

    pub(super) async fn scalar_value(&mut self) -> Result<Budgeted<Value>, SQLError> {
        uqa_core::value_image::ValueImageInput::read_from(&mut self.binary)
            .await
            .map_err(super::super::expressions::value_error)
    }

    async fn scalar_slots(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<Expr>, SQLError> {
        self.slots(count, memory, || Expr::Star).await
    }

    pub(super) async fn slots<T: Send>(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
        mut create: impl FnMut() -> T + Send,
    ) -> Result<Vec<T>, SQLError> {
        let mut values = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            values.push(create())?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }

    async fn expression_image(&mut self) -> Result<Budgeted<Expr>, SQLError> {
        if self.header(super::super::expressions::MAGIC).await? != 1 {
            return Err(invalid("stored scalar count"));
        }
        let value = self.expression().await?;
        self.binary.end().await?;
        Ok(value)
    }
}

impl Input<'_> {
    pub(super) fn scalar_value(&mut self) -> Result<Budgeted<Value>, SQLError> {
        self.fields(async |mut input| input.scalar_value().await)
    }

    pub(in super::super) fn expression_tag(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
    ) -> Result<Expr, SQLError> {
        let mut value = Expr::Star;
        self.fields(async |mut input| input.expression_tag(tag, memory, &mut value).await)?;
        Ok(value)
    }

    pub(super) fn items<T>(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
        mut read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<T, SQLError>,
    ) -> Result<Vec<T>, SQLError> {
        let mut values = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            values.push(read(self, memory)?)?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }
}

pub(in super::super) fn expression_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Expr>, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        Fields {
            binary,
            retained_children: &mut 0,
        }
        .expression_image()
        .await
    })
}

impl Expr {
    /// Retain incomplete native syntax, metadata and independently owned literals across source-byte and continuation-poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<Budgeted<Self>, SQLError>, SQLError> {
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                Fields {
                    binary: reader.input(),
                    retained_children: &mut 0,
                }
                .expression_image()
                .await
            },
        )?)
    }
}
