//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Matching affinities alone cannot validate a pair that disagrees about its transaction model or database incarnation.

use std::sync::Arc;
use uqa_storage::{
    KeyValueBatch, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, MemoryKeyValueStore,
    PersistentStorageSession, StorageBackendResult, StorageSessionAffinity,
    StorageTransactionModel,
};

struct AdvertisedStore {
    inner: MemoryKeyValueStore,
    model: StorageTransactionModel,
    affinity: Option<StorageSessionAffinity>,
}

impl KeyValueStore for AdvertisedStore {
    fn transaction_model(&self) -> StorageTransactionModel {
        self.model
    }

    fn transaction_affinity(&self) -> Option<StorageSessionAffinity> {
        self.affinity.clone()
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.inner.put(key, value)
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.inner.delete(key)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix)
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.inner.delete_prefix(prefix)
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        self.inner.batch()
    }

    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }
}

#[test]
fn versioned_pairs_require_matching_models_databases_and_reported_affinity() {
    let legacy = StorageTransactionModel::ProviderSerialized;
    let a = StorageTransactionModel::VersionedConcurrent {
        database: uqa_storage::mvcc::DatabaseId::from_bytes([1; 16]),
    };
    let b = StorageTransactionModel::VersionedConcurrent {
        database: uqa_storage::mvcc::DatabaseId::from_bytes([2; 16]),
    };
    for (catalog_model, backend_model, report_affinity, accepted) in [
        (legacy, legacy, false, true),
        (legacy, legacy, true, true),
        (a, a, true, true),
        (a, a, false, false),
        (a, b, true, false),
        (legacy, a, true, false),
        (a, legacy, true, false),
    ] {
        let affinity = report_affinity.then(StorageSessionAffinity::new);
        let store = |model| -> Arc<dyn KeyValueStore> {
            Arc::new(AdvertisedStore {
                inner: MemoryKeyValueStore::new(),
                model,
                affinity: affinity.clone(),
            })
        };
        let pair = PersistentStorageSession::new(
            Arc::new(KeyValueCatalog::new(store(catalog_model))),
            Arc::new(KeyValueStorageBackend::new(store(backend_model))),
        );
        assert_eq!(pair.validate_transaction_affinity().is_ok(), accepted);
        assert!(!pair.backend.in_transaction());
        assert_eq!(
            pair.backend.supports_concurrent_pinned_read_and_write(),
            backend_model.is_versioned()
        );
    }
}
