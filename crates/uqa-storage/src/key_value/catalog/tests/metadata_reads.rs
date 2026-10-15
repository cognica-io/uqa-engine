//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed metadata scans preserve literal selection and avoid payload copies.

use super::*;
use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use std::cell::Cell;

struct CountedRead<'a> {
    inner: &'a dyn KeyValueRead,
    control: &'a StorageReadControl,
    visited: &'a Cell<usize>,
}

impl KeyValueRead for CountedRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.inner.revision(prefixes)
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_value(key, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_prefix(prefix, &mut |key, value| {
            self.visited.set(self.visited.get() + 1);
            visit(key, value)
        })
    }
}

#[test]
fn fixed_length_metadata_visits_seek_only_selected_records() {
    for unrelated in [0, 1024] {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(store.clone());
        let prefix = "selected:%_\0日本語:";
        for suffix in ["aa", "bb"] {
            catalog
                .set_metadata(&format!("{prefix}{suffix}"), "value")
                .unwrap();
        }
        for index in 0..unrelated {
            catalog
                .set_metadata(&format!("outside:{index:04}"), "payload")
                .unwrap();
        }
        // A different encoded length shares the literal prefix but must not be read or decoded.
        store
            .put(
                &single_str_key(TAG_METADATA, &format!("{prefix}long")).unwrap(),
                &[0xff],
            )
            .unwrap();
        store
            .with_read_view(&mut |inner| {
                let visited = Cell::new(0);
                let read = CountedRead {
                    inner,
                    control: inner.control(),
                    visited: &visited,
                };
                let mut returned = 0;
                super::super::schema_table::visit_metadata(
                    &read,
                    prefix,
                    Some(prefix.len() + 2),
                    &mut |key, value| {
                        assert_eq!(key.len(), prefix.len() + 2);
                        assert_eq!(value, "value");
                        returned += 1;
                        Ok(())
                    },
                )?;
                assert_eq!((visited.get(), returned), (2, 2));
                assert_eq!(inner.control().memory().used(), 0);
                visited.set(0);
                super::super::schema_table::visit_metadata(
                    &read,
                    "absent:",
                    Some(9),
                    &mut |_, _| unreachable!(),
                )?;
                assert_eq!(visited.get(), 0);
                let zero = StorageReadControl::with_limit(0);
                let limited = CountedRead {
                    control: &zero,
                    ..read
                };
                let error = super::super::schema_table::visit_metadata(
                    &limited,
                    prefix,
                    Some(prefix.len() + 2),
                    &mut |_, _| unreachable!(),
                );
                assert!(matches!(error, Err(StorageBackendError::Memory(_))));
                assert_eq!(visited.get(), 0);
                Ok(())
            })
            .unwrap();
        let mut calls = 0;
        let error =
            catalog.visit_metadata_with_key_length(prefix, prefix.len() + 2, &mut |_, _| {
                calls += 1;
                Err(uqa_core::QueryCancelled::USER_REQUEST.into())
            });
        assert!(matches!(error, Err(StorageBackendError::Cancelled(_))));
        assert_eq!(calls, 1);
    }
}

#[test]
fn metadata_visitors_borrow_keys_and_payloads_without_corpus_allocations() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store);
    let prefix = "selected:%_\0日本語:";
    let payload = "x".repeat(16 * 1024);
    for index in 0..128 {
        catalog
            .set_metadata(&format!("{prefix}{index:04}"), &payload)
            .unwrap();
    }
    catalog.set_metadata("unrelated", "outside").unwrap();
    let mut count = 0;
    let allocation = allocation_counter::measure(|| {
        catalog
            .visit_metadata_with_prefix(prefix, &mut |key, value| {
                assert!(key.starts_with(prefix));
                assert_eq!(value, payload);
                count += 1;
                Ok(())
            })
            .unwrap();
    });
    assert_eq!(count, 128);
    assert_eq!(allocation.bytes_total, 0);
    catalog
        .visit_metadata_with_prefix("absent", &mut |_, _| panic!("absent prefix"))
        .unwrap();
}

#[test]
fn metadata_visitors_preserve_errors_and_validate_only_selected_values() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store.clone());
    for key in ["scope:a", "scope:b"] {
        catalog.set_metadata(key, "valid").unwrap();
    }
    store
        .put(&single_str_key(TAG_METADATA, "outside").unwrap(), &[0xff])
        .unwrap();
    let mut calls = 0;
    let result = catalog.visit_metadata_with_prefix("scope:", &mut |_, _| {
        calls += 1;
        Err(uqa_core::QueryCancelled::USER_REQUEST.into())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    assert_eq!(catalog.metadata_with_prefix("scope:").unwrap().len(), 2);
    assert!(catalog
        .visit_metadata_with_prefix("outside", &mut |_, _| panic!("invalid value"))
        .is_err());
}

#[test]
fn metadata_visitors_observe_provider_cancellation_after_the_last_record() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.set_metadata("one", "value").unwrap();
    let mut cancellation = None;
    store
        .with_read_view(&mut |read| {
            cancellation = Some(read.control().cancellation().clone());
            Ok(())
        })
        .unwrap();
    let cancellation = cancellation.unwrap();
    let result = catalog.visit_metadata_with_prefix("one", &mut |_, _| {
        cancellation.cancel();
        Ok(())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    let result = catalog.visit_metadata_with_prefix("absent", &mut |_, _| unreachable!());
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
}
