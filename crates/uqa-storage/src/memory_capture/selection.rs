//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable source membership belongs to the capture owner, before payload preservation.

use super::{MemoryCaptureKey, MemoryCaptureRegistry, Registry};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::sync::{Arc, Weak};
use uqa_core::memory::{Budgeted, BudgetedSharedMap, BudgetedSharedMapSnapshot, MemoryBudget};

/// An immutable set of source identities from one capture domain. Successors preserve earlier roots, own their construction allowance and retain no mutable source handles. The publishing owner keeps the selected sources alive until cut registration.
#[derive(Clone)]
pub struct MemoryCaptureSources {
    registry: Weak<Registry>,
    root: BudgetedSharedMapSnapshot<u64, ()>,
    memory: MemoryBudget,
}

impl MemoryCaptureRegistry {
    pub fn sources(&self, memory: &MemoryBudget) -> MemoryCaptureSources {
        MemoryCaptureSources {
            registry: Arc::downgrade(&self.0),
            root: BudgetedSharedMap::new(memory).snapshot(),
            memory: memory.clone(),
        }
    }
}

impl MemoryCaptureSources {
    pub fn len(&self) -> usize {
        self.root.len()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// Add an exact source incarnation under the membership owner's allowance. An admission failure leaves the original root unchanged; adding an existing identity allocates nothing.
    pub fn with_source<F: Send + Sync + 'static>(
        &self,
        key: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<Self> {
        self.check_domain(key)?;
        if self.root.get(&key.id).is_some() {
            return Ok(self.clone());
        }
        let mut map = BudgetedSharedMap::new(&self.memory);
        map.restore(&self.root);
        let root = map.with_insert(key.id, ())?.snapshot();
        Ok(Self {
            root,
            ..self.clone()
        })
    }

    /// Remove only this incarnation. Previously published selections and cuts retain their original membership.
    pub fn without_source<F: Send + Sync + 'static>(
        &self,
        key: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<Self> {
        self.check_domain(key)?;
        let mut map = BudgetedSharedMap::new(&self.memory);
        map.restore(&self.root);
        let root = map.with_remove(&key.id)?.snapshot();
        Ok(Self {
            root,
            ..self.clone()
        })
    }

    fn check_domain<F: Send + Sync + 'static>(
        &self,
        key: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<()> {
        if !Weak::ptr_eq(&self.registry, &key.registry) {
            return Err(StorageBackendError::Other(
                "source belongs to another memory capture domain".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn admit(
        &self,
        registry: &MemoryCaptureRegistry,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SelectedSources> {
        control.check()?;
        if !Weak::ptr_eq(&self.registry, &Arc::downgrade(&registry.0)) {
            return Err(StorageBackendError::Other(
                "selection belongs to another memory capture domain".into(),
            ));
        }
        let memory = control.memory().reserve(self.root.structural_bytes()?)?;
        Ok(SelectedSources::Exact(Budgeted::new(
            self.root.clone(),
            memory,
        )))
    }
}

pub(super) enum SelectedSources {
    All,
    Exact(Budgeted<BudgetedSharedMapSnapshot<u64, ()>>),
}

impl SelectedSources {
    pub(super) fn contains(&self, id: u64) -> bool {
        match self {
            Self::All => true,
            Self::Exact(root) => root.get(&id).is_some(),
        }
    }
}

pub(super) fn not_selected() -> StorageBackendError {
    StorageBackendError::Other("source is not selected by this memory capture".into())
}
