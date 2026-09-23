//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use uqa_core::memory::MemoryReservation;
use uqa_storage::read_control::StorageReadControl;

use crate::{Result, SQLiteError};

#[derive(Default)]
struct State {
    admitted: bool,
    owners: usize,
}

type Owners = HashMap<PathBuf, Arc<Mutex<State>>>;
static OWNERS: OnceLock<Mutex<Owners>> = OnceLock::new();

pub(super) struct Admission(Arc<Mutex<State>>);

struct Lease {
    state: Arc<Mutex<State>>,
    _memory: MemoryReservation,
}

impl Admission {
    pub(super) fn acquire(
        path: &Path,
        exclusive: bool,
        control: &StorageReadControl,
    ) -> Result<Self> {
        control.check()?;
        let mut registry = OWNERS.get_or_init(Mutex::default).lock();
        registry.retain(|_, state| Arc::strong_count(state) > 1);
        let shared = Arc::clone(registry.entry(path.to_owned()).or_default());
        let mut state = shared.lock();
        if state.admitted || (exclusive && state.owners != 0) {
            return Err(SQLiteError::DatabaseRestoreBusy);
        }
        state.admitted = true;
        drop(state);
        Ok(Self(shared))
    }

    pub(super) fn retain(&self, control: &StorageReadControl) -> Result<Box<dyn Send + Sync>> {
        control.check()?;
        let memory = control.memory().reserve(std::mem::size_of::<Lease>())?;
        let mut state = self.0.lock();
        state.owners = state
            .owners
            .checked_add(1)
            .ok_or_else(|| SQLiteError::StorageBackend("database owner count exhausted".into()))?;
        Ok(Box::new(Lease {
            state: Arc::clone(&self.0),
            _memory: memory,
        }))
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.0.lock().admitted = false;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.state.lock().owners -= 1;
    }
}
