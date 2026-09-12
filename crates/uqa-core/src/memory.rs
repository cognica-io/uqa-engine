//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared byte allowances with reservations that follow allocation ownership.
//!
//! Owners reserve requested buffer layouts before allocating. Allocator bookkeeping, borrowed data, and separately owned immutable resources are outside this allowance.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod deque;
mod vec;

pub use deque::BudgetedDeque;
pub use vec::BudgetedVec;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory requires {required} bytes, exceeding limit {limit}")]
    Limit { required: usize, limit: usize },
    #[error("memory allocation size overflow")]
    SizeOverflow,
    #[error("memory allocation failed: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
}

#[derive(Debug)]
struct Allowance {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}

/// Clones share one allowance, including reservations retained by completed producers.
#[derive(Debug, Clone)]
pub struct MemoryBudget(Arc<Allowance>);

impl MemoryBudget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Allowance {
            limit,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }))
    }

    pub fn limit(&self) -> usize {
        self.0.limit
    }

    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Relaxed)
    }

    /// Largest simultaneous reservation, including old and replacement buffers.
    pub fn peak(&self) -> usize {
        self.0.peak.load(Ordering::Relaxed)
    }

    pub fn reserve(&self, bytes: usize) -> Result<MemoryReservation, MemoryError> {
        let mut reservation = self.empty_reservation();
        reservation.grow(bytes)?;
        Ok(reservation)
    }

    pub fn empty_reservation(&self) -> MemoryReservation {
        MemoryReservation {
            budget: self.clone(),
            bytes: 0,
        }
    }
}

/// A unique lease: release it only after the associated allocation has been freed.
#[derive(Debug)]
pub struct MemoryReservation {
    budget: MemoryBudget,
    bytes: usize,
}

impl MemoryReservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn budget(&self) -> &MemoryBudget {
        &self.budget
    }

    /// Reserve additional bytes atomically; a failed request leaves this lease unchanged.
    pub fn grow(&mut self, additional: usize) -> Result<(), MemoryError> {
        if additional == 0 {
            return Ok(());
        }
        let allowance = &self.budget.0;
        let mut used = allowance.used.load(Ordering::Relaxed);
        loop {
            let required = used
                .checked_add(additional)
                .ok_or(MemoryError::SizeOverflow)?;
            if required > allowance.limit {
                return Err(MemoryError::Limit {
                    required,
                    limit: allowance.limit,
                });
            }
            match allowance.used.compare_exchange_weak(
                used,
                required,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.bytes += additional;
                    allowance.peak.fetch_max(required, Ordering::Relaxed);
                    return Ok(());
                }
                Err(current) => used = current,
            }
        }
    }

    /// Transfer ownership without releasing and reacquiring the shared allowance.
    ///
    /// Panics if the leases belong to different allowances.
    pub fn absorb(&mut self, mut other: Self) {
        assert!(
            Arc::ptr_eq(&self.budget.0, &other.budget.0),
            "different memory allowances"
        );
        self.bytes += other.bytes;
        other.bytes = 0;
    }
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// An immutable result and its owned allocations. Cloning the value is a separate allocation.
#[derive(Debug)]
pub struct Budgeted<T> {
    // Field order ensures that the value is destroyed before its allowance is returned.
    value: T,
    memory: MemoryReservation,
}

impl<T> Budgeted<T> {
    /// The caller must associate all owned allocations with this reservation.
    pub fn new(value: T, memory: MemoryReservation) -> Self {
        Self { value, memory }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory.bytes()
    }

    /// Move the value and lease together to another allocation owner.
    pub fn into_parts(self) -> (T, MemoryReservation) {
        (self.value, self.memory)
    }
}

impl<T> std::ops::Deref for Budgeted<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

fn buffer_bytes<T>(capacity: usize) -> Result<usize, MemoryError> {
    capacity
        .checked_mul(std::mem::size_of::<T>())
        .filter(|bytes| isize::try_from(*bytes).is_ok())
        .ok_or(MemoryError::SizeOverflow)
}

fn replacement<T>(
    budget: &MemoryBudget,
    capacity: usize,
    required: usize,
) -> Result<(usize, MemoryReservation), MemoryError> {
    let preferred = capacity.saturating_mul(2).max(required);
    if let Ok(bytes) = buffer_bytes::<T>(preferred) {
        if let Ok(memory) = budget.reserve(bytes) {
            return Ok((preferred, memory));
        }
    }
    Ok((required, budget.reserve(buffer_bytes::<T>(required)?)?))
}

#[cfg(test)]
mod tests;
