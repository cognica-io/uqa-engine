//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live session cursor descriptions independent of the executor's current location.

use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use uqa_sql::{catalog::session::CursorMetadata, SQLError};

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<String, Weak<CursorMetadata>>,
    next_id: u64,
}

/// The session retains this registry; an operational portal retains its registration.
#[derive(Default)]
pub struct PortalRegistry {
    state: Arc<Mutex<RegistryState>>,
}

/// Moving an executor out of its session map does not hide or release its cursor name.
pub struct PortalRegistration {
    metadata: Arc<CursorMetadata>,
    registry: Weak<Mutex<RegistryState>>,
}

impl PortalRegistry {
    pub fn ensure_available(&self, name: &str) -> Result<(), SQLError> {
        ensure_available(&self.state.lock(), name)
    }

    pub fn register(&self, metadata: CursorMetadata) -> Result<PortalRegistration, SQLError> {
        let mut state = self.state.lock();
        ensure_available(&state, &metadata.name)?;
        let metadata = Arc::new(metadata);
        state
            .entries
            .insert(metadata.name.clone(), Arc::downgrade(&metadata));
        Ok(PortalRegistration {
            metadata,
            registry: Arc::downgrade(&self.state),
        })
    }

    pub fn snapshot(&self) -> Vec<CursorMetadata> {
        self.state
            .lock()
            .entries
            .values()
            .filter_map(Weak::upgrade)
            .map(|metadata| (*metadata).clone())
            .collect()
    }

    pub fn allocate_name(&self) -> String {
        let mut state = self.state.lock();
        loop {
            state.next_id = state.next_id.wrapping_add(1);
            let name = format!("<unnamed portal {}>", state.next_id);
            if ensure_available(&state, &name).is_ok() {
                return name;
            }
        }
    }
}

fn ensure_available(state: &RegistryState, name: &str) -> Result<(), SQLError> {
    if state
        .entries
        .get(name)
        .is_some_and(|entry| entry.strong_count() != 0)
    {
        return Err(SQLError::Routine {
            sqlstate: "42P03".into(),
            message: format!("cursor \"{name}\" already exists"),
        });
    }
    Ok(())
}

impl Drop for PortalRegistration {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock();
            if state
                .entries
                .get(&self.metadata.name)
                .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.metadata)))
            {
                state.entries.remove(&self.metadata.name);
            }
        }
    }
}

#[cfg(test)]
mod tests;
