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

mod publication;
pub use publication::{MemoryCaptureMember, MemoryCaptureMembership};

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

    fn empty(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            root: BudgetedSharedMap::new(&self.memory).snapshot(),
            memory: self.memory.clone(),
        }
    }

    /// Merge exact incarnations while constructing a publication generation. Capturing the resulting immutable root does not repeat this traversal.
    pub fn with_sources(&self, sources: &Self) -> StorageBackendResult<Self> {
        if !Weak::ptr_eq(&self.registry, &sources.registry) {
            return Err(StorageBackendError::Other(
                "sources belong to another memory capture domain".into(),
            ));
        }
        let mut next = self.clone();
        for (&id, ()) in &sources.root {
            next = next.with_identity(id)?;
        }
        Ok(next)
    }

    fn with_identity(&self, id: u64) -> StorageBackendResult<Self> {
        if self.root.get(&id).is_some() {
            return Ok(self.clone());
        }
        let mut map = BudgetedSharedMap::new(&self.memory);
        map.restore(&self.root);
        Ok(Self {
            root: map.with_insert(id, ())?.snapshot(),
            ..self.clone()
        })
    }

    fn without_identity(&self, id: u64) -> StorageBackendResult<Self> {
        let mut map = BudgetedSharedMap::new(&self.memory);
        map.restore(&self.root);
        Ok(Self {
            root: map.with_remove(&id)?.snapshot(),
            ..self.clone()
        })
    }

    /// Add an exact source incarnation under the membership owner's allowance. An admission failure leaves the original root unchanged; adding an existing identity allocates nothing.
    pub fn with_source<F: Send + Sync + 'static>(
        &self,
        key: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<Self> {
        self.check_domain(key)?;
        self.with_identity(key.id)
    }

    /// Remove only this incarnation. Previously published selections and cuts retain their original membership.
    pub fn without_source<F: Send + Sync + 'static>(
        &self,
        key: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<Self> {
        self.check_domain(key)?;
        self.without_identity(key.id)
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
    ) -> StorageBackendResult<Budgeted<BudgetedSharedMapSnapshot<u64, ()>>> {
        control.check()?;
        if !Weak::ptr_eq(&self.registry, &Arc::downgrade(&registry.0)) {
            return Err(StorageBackendError::Other(
                "selection belongs to another memory capture domain".into(),
            ));
        }
        let memory = control.memory().reserve(self.root.structural_bytes()?)?;
        Ok(Budgeted::new(self.root.clone(), memory))
    }
}

pub(super) enum SelectedSources {
    All,
    Exact(Budgeted<BudgetedSharedMapSnapshot<u64, ()>>),
    Union([Budgeted<BudgetedSharedMapSnapshot<u64, ()>>; 2]),
}

impl SelectedSources {
    pub(super) fn contains(&self, id: u64) -> bool {
        match self {
            Self::All => true,
            Self::Exact(root) => root.get(&id).is_some(),
            Self::Union(roots) => roots.iter().any(|root| root.get(&id).is_some()),
        }
    }
}

pub(super) fn not_selected() -> StorageBackendError {
    StorageBackendError::Other("source is not selected by this memory capture".into())
}
