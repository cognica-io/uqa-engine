//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional dispatch moves argument values while preserving the names needed by overload diagnostics and host routines.

use super::EvaluatedArguments;
use crate::error::Result;
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionSmallVec},
    Value,
};

pub(super) struct MovedArguments<'a> {
    pub(super) values: ProductionSmallVec<'a, [Value; 4]>,
    pairs: EvaluatedArguments,
    payload: Option<MemoryReservation>,
}

impl<'a> MovedArguments<'a> {
    pub(super) fn new(
        arguments: Produced<EvaluatedArguments>,
        control: &ProductionControl<'a>,
    ) -> Result<Self> {
        let mut values = ProductionSmallVec::new(*control);
        values.reserve(arguments.len())?;
        let (pairs, payload) = arguments.into_parts();
        let mut moved = Self {
            values,
            pairs,
            payload,
        };
        for (_, value) in &mut moved.pairs {
            moved.values.push(std::mem::replace(value, Value::Null))?;
        }
        control.check()?;
        Ok(moved)
    }

    /// Reuse the caller's original argument buffer when SQL needs its names and values together.
    pub(super) fn into_pairs(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<EvaluatedArguments>> {
        for (_, original) in self.pairs.iter_mut().rev() {
            control.check()?;
            *original = self
                .values
                .pop()
                .expect("one positional value per argument");
        }
        Ok(control.finish(self.pairs, self.payload)?)
    }
}

#[cfg(test)]
mod tests;
