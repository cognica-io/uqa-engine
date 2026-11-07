//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted native scalar construction keeps unfinished parents in an explicit owner.

use super::{invalid, Input};
use crate::{ast::Expr, SQLError};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken, Value,
};

mod children;
mod metadata;
mod nodes;
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

fn boxed<T>(value: T, memory: &mut MemoryReservation) -> Result<Box<T>, SQLError> {
    memory.grow(size_of::<T>())?;
    Ok(Box::new(value))
}

impl Input<'_> {
    pub(super) fn expression(&mut self) -> Result<Budgeted<Expr>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut parents = BudgetedVec::new(self.binary.memory);
        let mut frame = Frame {
            value: self.expression_node(&mut memory)?,
            cursor: 0,
        };
        loop {
            self.binary.cancellation.check()?;
            if frame.prepare_child(self)? {
                parents.push(frame)?;
                frame = Frame {
                    value: self.expression_node(&mut memory)?,
                    cursor: 0,
                };
            } else if let Some(mut parent) = parents.pop() {
                *parent.child().expect("suspended scalar child") = frame.value;
                parent.cursor += 1;
                frame = parent;
            } else {
                return Ok(Budgeted::new(frame.value, memory));
            }
        }
    }

    pub(super) fn scalar_value(&mut self) -> Result<Budgeted<Value>, SQLError> {
        uqa_core::value_image::read_value_image(
            self.binary.reader,
            self.binary.memory,
            self.binary.cancellation,
        )
        .map_err(super::super::expressions::value_error)
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

    fn scalar_slots(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<Expr>, SQLError> {
        self.items(count, memory, |_, _| Ok(Expr::Star))
    }
}

pub(in super::super) fn expression_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Expr>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(super::super::expressions::MAGIC)? != 1 {
        return Err(invalid("stored scalar count"));
    }
    let value = input.expression()?;
    input.binary.end()?;
    Ok(value)
}
