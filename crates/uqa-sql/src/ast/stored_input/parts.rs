//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::InputError;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// Completed child allocations transfer together when their containing declaration is built.
pub(crate) struct Parts<'c> {
    control: ProductionControl<'c>,
    memory: Option<MemoryReservation>,
}

impl<'c> Parts<'c> {
    pub fn new(control: ProductionControl<'c>) -> Self {
        Self {
            control,
            memory: control.empty_reservation(),
        }
    }

    pub fn take<T>(&mut self, value: Produced<T>) -> T {
        let (value, memory) = value.into_parts();
        self.memory = self.control.combine(self.memory.take(), memory);
        value
    }

    pub fn finish<T>(self, value: T) -> Result<Produced<T>, InputError> {
        Ok(self.control.finish(value, self.memory)?)
    }
}
