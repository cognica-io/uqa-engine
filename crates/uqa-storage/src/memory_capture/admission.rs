//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registration owns identity publication and fixed source admission as one transition.

use super::{
    allocate, CellOwner, MemoryCaptureCell, MemoryCaptureRegistry, MemoryCaptureSource,
    RegisteredCell, Registration, WAIT,
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use parking_lot::RwLock;
use std::sync::{atomic::Ordering, Arc};
use uqa_core::memory::Budgeted;

#[cfg(test)]
use super::tests;

pub(super) fn register<T: MemoryCaptureSource>(
    cell: &mut MemoryCaptureCell<T>,
    registry: &MemoryCaptureRegistry,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<()> {
    if let Some(control) = control {
        control.check()?;
    }
    if let Some(registration) = cell.registration() {
        return if Arc::ptr_eq(&registration.registry, &registry.0) {
            Ok(())
        } else {
            Err(StorageBackendError::Other(
                "memory capture source is already registered in another domain".into(),
            ))
        };
    }
    let _admission = if let Some(control) = control {
        loop {
            control.check()?;
            if let Some(guard) = registry.0.admission.try_read_recursive_for(WAIT) {
                break guard;
            }
            #[cfg(test)]
            tests::source_wait();
        }
    } else {
        registry.0.admission.read_recursive()
    };
    let memory = control
        .map(|control| {
            control
                .memory()
                .reserve(size_of::<Budgeted<RegisteredCell<T>>>())
        })
        .transpose()?;
    let registration = Registration {
        registry: registry.0.clone(),
        id: allocate(&registry.0.next_cell)?,
        born: registry.0.next_capture.load(Ordering::Relaxed),
    };
    if let Some(control) = control {
        control.check()?;
    }
    // All fallible work precedes taking the original value or publishing its identity.
    let CellOwner::Unregistered(value) = &mut cell.owner else {
        unreachable!("registered cells return before assigning another identity")
    };
    let value = value.take().expect("unpublished source value").into_inner();
    let registered = RegisteredCell {
        registration,
        value: RwLock::new(value),
    };
    cell.owner = match memory {
        Some(memory) => CellOwner::Admitted(Arc::new(Budgeted::new(registered, memory))),
        None => CellOwner::Registered(Arc::new(registered)),
    };
    Ok(())
}
