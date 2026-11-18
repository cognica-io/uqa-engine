//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed image hashes belong to the selected immutable source and registry mutation generation.

use super::{MemoryCapture, MemoryCaptureKey};
use crate::{
    backup::{BackupCut, UnitAddress, UnitFingerprint, UnitKind},
    StorageBackendResult,
};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, Weak,
};
use uqa_core::memory::{BudgetedDeque, BudgetedMap, MemoryBudget, MemoryReservation};

/// This optional source metadata has one fixed maximum across old cuts, active captures and current data. Replacing a slot loses an optimization and never changes an image.
const ENTRIES: usize = 1024;
type Cache = Arc<Mutex<CacheEntries>>;

#[derive(Clone)]
pub struct MemoryImageOwner {
    owner: Weak<dyn Send + Sync>,
    bytes: usize,
}

impl MemoryImageOwner {
    /// Identify an owner whose complete encoded contents stay immutable while its allocation is shared. The weak reference detects copy-on-write detachment and cannot pin the owner's payload.
    pub fn from_arc<T: Send + Sync + 'static>(value: &Arc<T>) -> Self {
        let erased: Arc<dyn Send + Sync> = value.clone();
        Self {
            owner: Arc::downgrade(&erased),
            bytes: size_of::<T>() + 2 * size_of::<usize>(),
        }
    }
}

enum Scope {
    Group(u64, u64),
    Owner {
        owner: MemoryImageOwner,
        _memory: MemoryReservation,
    },
}

struct CacheEntries {
    rows: BudgetedMap<[u8; 32], (Scope, UnitFingerprint)>,
    insertion: BudgetedDeque<[u8; 32]>,
}

impl CacheEntries {
    fn new() -> Self {
        let memory = MemoryBudget::new(256 << 10);
        Self {
            rows: BudgetedMap::new(&memory),
            insertion: BudgetedDeque::new(&memory),
        }
    }
}

#[derive(Default)]
pub(super) struct RegistryImages {
    generation: AtomicU64,
    cache: OnceLock<Cache>,
}

impl RegistryImages {
    pub(super) fn current(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    pub(super) fn invalidate(&self) {
        // The capture admission lock orders reads against writes; saturation disables future certificates without reusing an old generation.
        let _ = uqa_core::atomic::try_update_u64(
            &self.generation,
            Ordering::Relaxed,
            Ordering::Relaxed,
            |n| n.checked_add(1),
        );
    }
}

/// A selected source's immutable ownership scope. A generation is only a cache key; each reusable fingerprint comes from hashing the complete encoded unit at EOF.
#[derive(Clone)]
pub struct MemoryImageGeneration {
    cache: Cache,
    source: u64,
    generation: u64,
}

impl<F: Send + Sync + 'static> MemoryCaptureKey<F> {
    /// Select this key's source group from the admitted cut. The mapping owner must choose a group that changes whenever the unit's immutable inputs are replaced.
    pub fn image_generation(
        &self,
        cut: &MemoryCapture,
    ) -> StorageBackendResult<Option<MemoryImageGeneration>> {
        if !Weak::ptr_eq(&self.registry, &cut.0.registry) || self.born > cut.0.id {
            return Err(super::selection::not_selected());
        }
        let state = cut.0.state.lock();
        let sources = match &*state {
            super::State::Active { sources, .. } => sources,
            super::State::Failed(error) => return Err(super::failure::reproduce(error)),
        };
        if !sources.contains(self.id) {
            return Err(super::selection::not_selected());
        }
        let Some(registry) = self.registry.upgrade() else {
            return Ok(None);
        };
        let generation = cut.0.image_generation;
        Ok((generation != u64::MAX).then(|| MemoryImageGeneration {
            cache: registry
                .images
                .cache
                .get_or_init(|| Arc::new(Mutex::new(CacheEntries::new())))
                .clone(),
            source: self.id,
            generation,
        }))
    }
}

impl MemoryImageGeneration {
    pub(crate) fn address_key(
        cut: BackupCut,
        address: UnitAddress<'_>,
        kind: UnitKind,
    ) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(cut.source.as_bytes());
        hash.update([cut.format.provider() as u8]);
        hash.update(cut.format.version().to_be_bytes());
        hash.update(address.namespace.as_bytes());
        hash.update(address.generation.to_be_bytes());
        hash.update(address.family.to_be_bytes());
        hash.update([kind as u8]);
        hash.update(address.key);
        hash.finalize().into()
    }

    pub(crate) fn get(
        &self,
        key: &[u8; 32],
        owner: Option<&MemoryImageOwner>,
    ) -> Option<UnitFingerprint> {
        let cache = self.cache.lock();
        let (scope, fingerprint) = cache.rows.get(key)?;
        let valid = match (scope, owner) {
            (Scope::Group(id, version), None) => *id == self.source && *version == self.generation,
            (Scope::Owner { owner: saved, .. }, Some(current)) => {
                Weak::ptr_eq(&saved.owner, &current.owner)
            }
            _ => false,
        };
        valid.then_some(*fingerprint)
    }

    pub(crate) fn observe(
        &self,
        key: [u8; 32],
        owner: Option<&MemoryImageOwner>,
        fingerprint: UnitFingerprint,
    ) {
        let mut cache = self.cache.lock();
        // Reclaim at most one old entry per observation. Expensive weak headers remain optional even when the cache has room for another fixed-size digest.
        if cache.rows.len() == ENTRIES
            || cache.rows.budget().available()
                < owner.map_or(0, |owner| owner.bytes).saturating_add(256)
        {
            if let Some(oldest) = cache.insertion.pop_front() {
                cache.rows.remove(&oldest);
            }
        }
        let scope = if let Some(owner) = owner {
            let Ok(memory) = cache.rows.budget().reserve(owner.bytes) else {
                return;
            };
            Scope::Owner {
                owner: owner.clone(),
                _memory: memory,
            }
        } else {
            Scope::Group(self.source, self.generation)
        };
        if let Some(existing) = cache.rows.get_mut(&key) {
            *existing = (scope, fingerprint);
            return;
        }
        if cache.insertion.reserve(1).is_ok()
            && cache.rows.insert(key, (scope, fingerprint)).is_ok()
        {
            cache
                .insertion
                .push_back(key)
                .expect("reserved fingerprint order entry");
        }
    }
}
