//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporary positional membership admits each node under the invoking request.

use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::SQLError;

pub(super) struct Demand<'c> {
    positions: ProductionMap<'c, usize, ()>,
    pub control: ProductionControl<'c>,
}

impl<'c> Demand<'c> {
    pub fn new(control: ProductionControl<'c>) -> Self {
        Self {
            positions: ProductionMap::new(control),
            control,
        }
    }

    pub fn contains(&self, position: usize) -> bool {
        self.positions.get(&position).is_some()
    }

    pub fn extend(&mut self, positions: impl IntoIterator<Item = usize>) -> Result<(), SQLError> {
        self.control.check()?;
        for position in positions {
            self.positions.insert_copy(position, ())?;
        }
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = &usize> {
        self.positions.iter().map(|(position, ())| position)
    }

    pub fn copy(&self) -> Result<Self, SQLError> {
        let mut output = Self::new(self.control);
        output.extend(self.iter().copied())?;
        Ok(output)
    }
}
