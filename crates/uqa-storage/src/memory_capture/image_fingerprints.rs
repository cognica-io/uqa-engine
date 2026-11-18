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
use uqa_core::memory::{BudgetedMap, MemoryBudget, MemoryReservation};

/// This optional source metadata has one fixed maximum across old cuts, active captures and current data. Replacing a slot loses an optimization and never changes an image.
const ENTRIES: usize = 1024;
const CACHE_BYTES: usize = 256 << 10;
type Cache = Arc<Mutex<BudgetedMap<[u8; 32], (Scope, UnitFingerprint)>>>;

fn new_cache() -> Cache {
    Arc::new(Mutex::new(BudgetedMap::new(&MemoryBudget::new(
        CACHE_BYTES,
    ))))
}

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
            cache: registry.images.cache.get_or_init(new_cache).clone(),
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
        let (scope, fingerprint) = cache.get(key)?;
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
        // Keep a stable sample by address hash. Sequential scans larger than the cache must not evict every reusable unit before they reach it.
        if cache.len() == ENTRIES && !cache.contains_key(&key) {
            let lowest_key = *cache.iter().next().expect("full fingerprint cache").0;
            if key < lowest_key {
                return;
            }
            cache.remove(&lowest_key);
        }
        let scope = if let Some(owner) = owner {
            let Ok(memory) = cache.budget().reserve(owner.bytes) else {
                return;
            };
            Scope::Owner {
                owner: owner.clone(),
                _memory: memory,
            }
        } else {
            Scope::Group(self.source, self.generation)
        };
        let _ = cache.insert(key, (scope, fingerprint));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::ImageDigest;

    #[test]
    fn bounded_fingerprint_cache_rejects_evicted_and_replaced_owners() {
        let first = MemoryImageGeneration {
            cache: new_cache(),
            source: 1,
            generation: 3,
        };
        let later = MemoryImageGeneration {
            generation: 4,
            ..first.clone()
        };
        let fp = UnitFingerprint {
            digest: ImageDigest::of_bytes(b"payload"),
            decoded_bytes: 7,
        };
        let key = [0; 32];
        first.observe(key, None, fp);
        assert_eq!(first.get(&key, None), Some(fp));
        assert_eq!(later.get(&key, None), None);
        let owner = Arc::new(vec![7; 65536]);
        let weak = MemoryImageOwner::from_arc(&owner);
        first.observe(key, Some(&weak), fp);
        assert_eq!(later.get(&key, Some(&weak)), Some(fp));
        drop(owner);
        assert!(weak.owner.upgrade().is_none());
        let different = Arc::new(vec![7; 65536]);
        assert_eq!(
            later.get(&key, Some(&MemoryImageOwner::from_arc(&different))),
            None
        );

        for number in 1..=ENTRIES {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&(number as u64).to_be_bytes());
            first.observe(key, None, fp);
        }
        assert_eq!(first.get(&key, Some(&weak)), None);
        let cache = first.cache.lock();
        assert_eq!(cache.len(), ENTRIES);
        assert!(cache.budget().used() <= CACHE_BYTES);
        drop(cache);
        let mut reused = 0;
        for number in 0..=ENTRIES {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&(number as u64).to_be_bytes());
            if first.get(&key, None).is_some() {
                reused += 1;
            } else {
                first.observe(key, None, fp);
            }
        }
        assert_eq!(reused, ENTRIES);
    }
}
