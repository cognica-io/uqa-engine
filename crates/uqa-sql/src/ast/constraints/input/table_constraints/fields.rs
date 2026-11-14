//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each stored table field retains its original payload reservation until transfer.

use crate::ast::{stored_input::InputError, TableConstraintSet};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

#[derive(Debug)]
pub(crate) struct ParsedTableConstraints {
    pub value: TableConstraintSet,
    pub memory: TableConstraintAllocations,
}

#[derive(Debug)]
pub(crate) struct TableConstraintAllocations {
    pub dropped_attributes: MemoryReservation,
    pub checks: MemoryReservation,
    pub foreign_keys: MemoryReservation,
    pub key_constraints: MemoryReservation,
    pub hierarchy: MemoryReservation,
    pub row_type_array_name: MemoryReservation,
}

impl ParsedTableConstraints {
    pub fn new(control: &ProductionControl<'_>) -> Result<Self, InputError> {
        control.check()?;
        let memory = control.budget().expect("admitted stored table input");
        Ok(Self {
            value: TableConstraintSet::default(),
            memory: TableConstraintAllocations {
                dropped_attributes: memory.empty_reservation(),
                checks: memory.empty_reservation(),
                foreign_keys: memory.empty_reservation(),
                key_constraints: memory.empty_reservation(),
                hierarchy: memory.empty_reservation(),
                row_type_array_name: memory.empty_reservation(),
            },
        })
    }

    pub fn into_produced(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<TableConstraintSet>, InputError> {
        let mut memory = self.memory.dropped_attributes;
        memory.absorb(self.memory.checks);
        memory.absorb(self.memory.foreign_keys);
        memory.absorb(self.memory.key_constraints);
        memory.absorb(self.memory.hierarchy);
        memory.absorb(self.memory.row_type_array_name);
        Ok(control.finish(self.value, Some(memory))?)
    }
}

pub(super) fn retain<T>(target: &mut MemoryReservation, value: Produced<T>) -> T {
    let (value, memory) = value
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted stored table field"))
        .into_parts();
    target.absorb(memory);
    value
}
