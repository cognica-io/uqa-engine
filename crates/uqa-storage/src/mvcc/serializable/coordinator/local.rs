//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local admission and participants retain the exclusively owned physical database together.

use std::sync::Arc;

use parking_lot::Mutex;

use super::{SerializableLeases, SerializableParticipant, SerializableTransactionId};
use crate::{
    mvcc::{LocalSerializableLeases, VersionResult},
    read_control::StorageReadControl,
};

/// One admission gate for every adapter over an exclusively owned database. Live participants retain both this gate and the physical owner, preventing adapter churn from replacing their liveness registry. The provider separately persists the graph and allocation watermark.
#[derive(Default)]
pub struct LocalSerializableState(Mutex<Option<Arc<LocalSerializableLeases>>>);

impl LocalSerializableState {
    /// Wait cancellably for shared admission and invoke the operation once. Lease release uses a separate registry lock, so the final participant may be dropped inside this operation without deadlocking. The borrowed owner remains retained throughout admission; native final-close work can run only afterward. The physical owner must enforce exclusive access by other processes.
    pub fn with_admission<O: Send + Sync + 'static, T>(
        self: &Arc<Self>,
        owner: &Arc<O>,
        control: &StorageReadControl,
        operation: impl FnOnce(&dyn SerializableLeases) -> VersionResult<T>,
    ) -> VersionResult<T> {
        let mut held = loop {
            control.cancellation().check()?;
            if let Some(held) = self.0.try_lock() {
                break held;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        control.cancellation().check()?;
        let registry =
            held.get_or_insert_with(|| Arc::new(LocalSerializableLeases::new(control.memory())));
        operation(&RetainedLeases {
            state: self,
            registry,
            owner,
        })
    }
}

struct RetainedLeases<'a, O> {
    state: &'a Arc<LocalSerializableState>,
    registry: &'a Arc<LocalSerializableLeases>,
    owner: &'a Arc<O>,
}

impl<O: Send + Sync + 'static> SerializableLeases for RetainedLeases<'_, O> {
    fn retain(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableParticipant> {
        self.registry.retain_with(
            id,
            (Arc::clone(self.state), Arc::clone(self.owner)),
            control,
        )
    }

    fn is_alive(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        control.cancellation().check()?;
        Ok(self.registry.is_alive(id))
    }

    fn reclaim(&self) {
        self.registry.reclaim();
    }
}

#[cfg(test)]
mod tests;
