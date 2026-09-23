//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted producers share constructors while output and temporary leases stay separate.

use super::{Budgeted, MemoryBudget, MemoryReservation};
use crate::{CancellationToken, Value, ValueRetentionError};

mod string;
mod vec;
pub use string::ProductionString;
pub use vec::ProductionVec;

/// Allocation and cancellation inputs borrowed for one production call. An ordinary call has no allowance or cancellation scope; a controlled call preserves both the original retained owner and the invoking reader.
#[derive(Clone, Copy)]
pub struct ProductionControl<'a> {
    budget: Option<&'a MemoryBudget>,
    original: Option<&'a CancellationToken>,
    invoking: Option<&'a CancellationToken>,
}

impl<'a> ProductionControl<'a> {
    pub const fn uncontrolled() -> Self {
        Self {
            budget: None,
            original: None,
            invoking: None,
        }
    }

    pub fn new(
        budget: &'a MemoryBudget,
        original: &'a CancellationToken,
        invoking: &'a CancellationToken,
    ) -> Self {
        Self {
            budget: Some(budget),
            original: Some(original),
            invoking: Some(invoking),
        }
    }

    pub fn budget(&self) -> Option<&'a MemoryBudget> {
        self.budget
    }

    pub fn check_cancellation(&self) -> Result<(), crate::QueryCancelled> {
        if let Some(original) = self.original {
            original.check()?;
        }
        if let Some(invoking) = self.invoking {
            invoking.check()?;
        }
        Ok(())
    }

    pub fn check(&self) -> Result<(), ValueRetentionError> {
        self.check_cancellation().map_err(Into::into)
    }

    pub fn empty_reservation(&self) -> Option<MemoryReservation> {
        self.budget.map(MemoryBudget::empty_reservation)
    }

    /// Admit an explicitly known allocation layout before its constructor runs.
    pub fn reserve(&self, bytes: usize) -> Result<Option<MemoryReservation>, ValueRetentionError> {
        self.check()?;
        self.budget
            .map(|budget| budget.reserve(bytes).map_err(Into::into))
            .transpose()
    }

    /// Transfer two leases for allocations already owned by the same composite result. This does not produce, copy or replace a value.
    pub fn combine(
        &self,
        left: Option<MemoryReservation>,
        right: Option<MemoryReservation>,
    ) -> Option<MemoryReservation> {
        self.assert_owner(left.as_ref());
        self.assert_owner(right.as_ref());
        match (left, right) {
            (Some(mut left), Some(right)) => {
                left.absorb(right);
                Some(left)
            }
            (None, None) => None,
            _ => unreachable!("production ownership modes were checked"),
        }
    }

    /// Associate a value with leases acquired by its constructors. Every owned allocation must already belong to the supplied reservation. The explicit pair prevents a representation-changing closure from silently introducing new payloads; callers must construct and admit those payloads at their owner.
    pub fn finish<T>(
        &self,
        value: T,
        memory: Option<MemoryReservation>,
    ) -> Result<Produced<T>, ValueRetentionError> {
        let output = Produced { value, memory };
        self.assert_owner(output.memory.as_ref());
        self.check()?;
        Ok(output)
    }

    pub fn copy_value(&self, value: &Value) -> Result<Produced<Value>, ValueRetentionError> {
        self.check()?;
        let result = match self.budget {
            Some(budget) => Produced::from(value.clone_budgeted_with_check(budget, || {
                if let Some(original) = self.original {
                    original.check()?;
                }
                if let Some(invoking) = self.invoking {
                    invoking.check()?;
                }
                Ok(())
            })?),
            None => Produced {
                value: value.clone(),
                memory: None,
            },
        };
        self.check()?;
        Ok(result)
    }

    pub fn copy_text(&self, text: &str) -> Result<Produced<String>, ValueRetentionError> {
        self.check()?;
        let mut output = ProductionString::new(*self);
        output.reserve(text.len())?;
        output.push_str(text)?;
        output.finish()
    }

    pub fn format(
        &self,
        arguments: std::fmt::Arguments<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        use std::fmt::Write;
        self.check()?;
        let mut output = ProductionString::new(*self);
        let formatted = output.write_fmt(arguments);
        let result = output.finish()?;
        assert!(
            formatted.is_ok(),
            "formatter failed without a production error"
        );
        Ok(result)
    }

    fn assert_owner(&self, memory: Option<&MemoryReservation>) {
        match (self.budget, memory) {
            (Some(budget), Some(memory)) => assert!(
                budget.shares_allowance(memory.budget()),
                "different production allowance"
            ),
            (None, None) => {}
            _ => panic!("controlled and uncontrolled production ownership must not be mixed"),
        }
    }
}

/// A produced value with its existing controlled lease, or an explicit ordinary result. There is no mutable dereference or generic map operation that could replace it with newly allocated, unadmitted payloads.
#[derive(Debug)]
pub struct Produced<T> {
    value: T,
    memory: Option<MemoryReservation>,
}

impl<T> Produced<T> {
    pub fn reserved_bytes(&self) -> usize {
        self.memory.as_ref().map_or(0, MemoryReservation::bytes)
    }

    /// Transfer the explicit value/lease pair to its next allocation owner. The caller must retain the lease until those allocations are freed or handed off under a documented legacy API boundary.
    pub fn into_parts(self) -> (T, Option<MemoryReservation>) {
        (self.value, self.memory)
    }

    /// Extract an ordinary result. A controlled result is returned unchanged instead of silently releasing its lease.
    pub fn into_uncontrolled(self) -> Result<T, Self> {
        if self.memory.is_some() {
            return Err(self);
        }
        Ok(self.value)
    }

    /// Preserve a controlled result in the existing budgeted carrier. An ordinary result is returned unchanged instead of being retroactively charged.
    pub fn into_budgeted(self) -> Result<Budgeted<T>, Self> {
        let Some(memory) = self.memory else {
            return Err(self);
        };
        Ok(Budgeted::new(self.value, memory))
    }
}

impl<T: Copy> Produced<Vec<T>> {
    /// Mutate fixed-length scratch slots without changing capacity or introducing owned payloads. The Copy bound excludes allocation-owning values and the slice cannot grow the admitted container.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.value.as_mut_slice()
    }
}

impl<T> From<Budgeted<T>> for Produced<T> {
    fn from(value: Budgeted<T>) -> Self {
        let (value, memory) = value.into_parts();
        Self {
            value,
            memory: Some(memory),
        }
    }
}

impl<T> std::ops::Deref for Produced<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests;
