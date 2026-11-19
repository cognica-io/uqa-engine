//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One allowance can share the admission of the same immutable storage generation.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_core::memory::{MemoryError, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Debug, Default)]
pub(crate) struct RetainedMemory(Mutex<Weak<MemoryReservation>>);

impl RetainedMemory {
    pub(crate) fn invalidate(&mut self) {
        *self.0.get_mut() = Weak::new();
    }

    pub(crate) fn with_retained<T>(
        &self,
        bytes: u128,
        control: &StorageReadControl,
        build: impl FnOnce(Arc<MemoryReservation>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        control.check()?;
        let mut cached = self.0.lock();
        control.check()?;
        let memory = if let Some(memory) = cached
            .upgrade()
            .filter(|memory| memory.budget().shares_allowance(control.memory()))
        {
            memory
        } else {
            let bytes = bytes
                .checked_add(size_of::<MemoryReservation>() as u128)
                .ok_or(MemoryError::SizeOverflow)?;
            let bytes = usize::try_from(bytes).map_err(|_| MemoryError::SizeOverflow)?;
            Arc::new(control.memory().reserve(bytes)?)
        };
        let result = build(Arc::clone(&memory))?;
        control.check()?;
        // Failed construction must not replace an existing generation lease.
        *cached = Arc::downgrade(&memory);
        Ok(result)
    }
}
