//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep conversion space charged between resident mutations and lend its leases to the disk builder.

use parking_lot::Mutex;
use std::sync::{Arc, Weak};
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation};

pub(super) struct Workspace {
    memory: Mutex<MemoryReservation>,
}

impl Workspace {
    pub(super) fn new(memory: MemoryReservation) -> Arc<Self> {
        Arc::new(Self {
            memory: Mutex::new(memory),
        })
    }

    pub(super) fn ensure(&self, bytes: usize) -> Result<(), MemoryError> {
        let bytes = bytes
            .checked_add(size_of::<Self>())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.memory.lock();
        let additional = bytes.saturating_sub(memory.bytes());
        memory.grow(additional)
    }

    fn take(&self, bytes: usize) -> Result<MemoryReservation, MemoryError> {
        let mut memory = self.memory.lock();
        let required = bytes
            .checked_add(size_of::<Self>())
            .ok_or(MemoryError::SizeOverflow)?;
        let additional = required.saturating_sub(memory.bytes());
        memory.grow(additional)?;
        Ok(memory.split(bytes))
    }
}

/// Failed construction returns every lease to the still-resident root. Once that root publishes disk pages and releases its workspace, only the surviving page allocations remain charged.
pub(super) struct Reservation {
    memory: Option<MemoryReservation>,
    owner: Weak<Workspace>,
}

impl Reservation {
    pub(super) fn reserve(
        memory: &MemoryBudget,
        owner: Option<&Arc<Workspace>>,
        bytes: usize,
    ) -> Result<Self, MemoryError> {
        Ok(Self {
            memory: Some(match owner {
                Some(owner) => owner.take(bytes)?,
                None => memory.reserve(bytes)?,
            }),
            owner: owner.map_or_else(Weak::new, Arc::downgrade),
        })
    }

    pub(super) fn grow(&mut self, bytes: usize) -> Result<(), MemoryError> {
        let memory = self.memory.as_mut().expect("live allocation lease");
        if let Some(owner) = self.owner.upgrade() {
            memory.absorb(owner.take(bytes)?);
            Ok(())
        } else {
            memory.grow(bytes)
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner
                .memory
                .lock()
                .absorb(self.memory.take().expect("live allocation lease"));
        }
    }
}
