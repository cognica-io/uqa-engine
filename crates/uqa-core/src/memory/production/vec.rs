//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Produced, ProductionControl};
use crate::{
    memory::{BudgetedVec, MemoryError, MemoryReservation},
    ValueRetentionError,
};

enum Buffer<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

/// Admitted vector capacity and element payloads have separate leases until the completed result combines them. Copy-only scratch elements need no payload owner; owned elements enter through `push_produced`.
pub struct ProductionVec<'a, T> {
    buffer: Buffer<T>,
    elements: Option<MemoryReservation>,
    control: ProductionControl<'a>,
}

impl<'a, T> ProductionVec<'a, T> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        let buffer = control.budget().map_or_else(
            || Buffer::Ordinary(Vec::new()),
            |budget| Buffer::Controlled(BudgetedVec::new(budget)),
        );
        Self {
            buffer,
            elements: control.empty_reservation(),
            control,
        }
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        match &mut self.buffer {
            Buffer::Ordinary(values) => {
                values.try_reserve(additional).map_err(MemoryError::from)?;
            }
            Buffer::Controlled(values) => values.reserve(additional)?,
        }
        Ok(())
    }

    pub fn push_produced(&mut self, value: Produced<T>) -> Result<(), ValueRetentionError> {
        self.control.assert_owner(value.memory.as_ref());
        self.reserve(1)?;
        let (value, memory) = value.into_parts();
        match &mut self.buffer {
            Buffer::Ordinary(values) => values.push(value),
            Buffer::Controlled(values) => values.push(value)?,
        }
        self.elements = self.control.combine(self.elements.take(), memory);
        Ok(())
    }

    pub fn finish(self) -> Result<Produced<Vec<T>>, ValueRetentionError> {
        let (value, memory) = match self.buffer {
            Buffer::Ordinary(value) => (value, None),
            Buffer::Controlled(value) => {
                let (value, memory) = value.into_parts();
                (value, Some(memory))
            }
        };
        self.control
            .finish(value, self.control.combine(memory, self.elements))
    }
}

impl<T: Copy> ProductionVec<'_, T> {
    /// Edit admitted scratch slots without growing the buffer or introducing allocation-owning elements.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match &mut self.buffer {
            Buffer::Ordinary(values) => values,
            Buffer::Controlled(values) => values,
        }
    }

    /// Append copy-only elements in bounded chunks, admitting capacity before copying and checking both cancellation scopes between chunks.
    pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        for chunk in values.chunks((4096 / std::mem::size_of::<T>().max(1)).max(1)) {
            self.reserve(chunk.len())?;
            match &mut self.buffer {
                Buffer::Ordinary(values) => values.extend_from_slice(chunk),
                Buffer::Controlled(values) => values.extend_from_slice(chunk)?,
            }
        }
        self.control.check()?;
        Ok(())
    }

    pub fn push_copy(&mut self, value: T) -> Result<(), ValueRetentionError> {
        self.reserve(1)?;
        match &mut self.buffer {
            Buffer::Ordinary(values) => values.push(value),
            Buffer::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }
}

impl<T> std::ops::Deref for ProductionVec<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match &self.buffer {
            Buffer::Ordinary(values) => values,
            Buffer::Controlled(values) => values,
        }
    }
}
