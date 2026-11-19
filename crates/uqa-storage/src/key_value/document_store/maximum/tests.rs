//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::{KeyValueReadRevision, RecordKeyReadVisitor};
use crate::read_control::{KeyValueReadVisitor, ValueReadVisitor};
use crate::{DocumentStore, KeyValueStore, MemoryKeyValueStore};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn key_value_maximum_does_not_collect_visible_document_ids() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store, "docs");
    for count in [32, 4096] {
        for id in 0..=count {
            documents.put(id, BTreeMap::new()).unwrap();
        }
        documents.delete(count).unwrap();
        let selected = documents.snapshot().unwrap();
        let expected = selected.doc_ids().unwrap().into_iter().max().unwrap();
        let mut actual = 0;
        let allocation = allocation_counter::measure(|| {
            actual = selected.max_doc_id().unwrap();
        });
        assert_eq!(actual, expected);
        assert_eq!(actual, count - 1);
        assert!(allocation.bytes_max < 1024, "{allocation:?}");
        assert_eq!(allocation.bytes_current, 0);
    }
}

#[test]
fn key_value_maximum_rejects_unversioned_capture_without_copying_records() {
    let store = Arc::new(MemoryKeyValueStore::new());
    store.put(b"unrelated", &[0; 128]).unwrap();
    let documents = KeyValueDocumentStore::new(store, "docs");
    let control = StorageReadControl::with_limit(0);
    assert!(matches!(
        documents.begin_max_doc_id_input(128, &control),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

struct Read {
    control: StorageReadControl,
    slots: Vec<(Vec<u8>, bool)>,
    visits: Arc<AtomicUsize>,
}

impl KeyValueRead for Read {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("no revision scan")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("no value read")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("no value scan")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        _: &StorageReadControl,
        visit: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert_eq!(limit, 1);
        if let Some((key, live)) = self.slots.iter().find(|(key, _)| {
            key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after)
        }) {
            self.visits.fetch_add(1, Ordering::SeqCst);
            visit(key, *live)?;
        }
        Ok(())
    }
}

#[test]
fn key_value_maximum_consumes_each_tombstone_without_reading_payloads() {
    let visits = Arc::new(AtomicUsize::new(0));
    let read = Arc::new(Read {
        control: StorageReadControl::with_limit(4096),
        slots: [(0, true), (5, true), (9, false), (100, false)]
            .map(|(id, live)| {
                (
                    crate::key_value::codec::document_key("docs", id).unwrap(),
                    live,
                )
            })
            .into(),
        visits: visits.clone(),
    });
    let weak = Arc::downgrade(&read);
    let documents = KeyValueDocumentStore {
        source: Source::Retained(read),
        table: "docs".to_owned().into(),
    };
    let control = StorageReadControl::with_limit(4096);
    let mut input = documents.begin_max_doc_id_input(17, &control).unwrap();
    drop(documents);
    assert!(weak.upgrade().is_some());
    for count in 1..=4 {
        assert_eq!(input.advance().unwrap(), MaximumDocIdStep::Pending);
        assert_eq!(visits.load(Ordering::SeqCst), count);
        assert!(control.memory().used() < 1024);
    }
    assert_eq!(input.advance().unwrap(), MaximumDocIdStep::Complete(5));
    assert!(weak.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_value_maximum_failure_retires_its_selected_key_and_read_owner() {
    for mode in 0..3 {
        let mut key = crate::key_value::codec::document_key("docs", 7).unwrap();
        if mode == 1 {
            key.pop();
        }
        let read = Arc::new(Read {
            control: StorageReadControl::with_limit(4096),
            slots: vec![(key, true)],
            visits: Arc::new(AtomicUsize::new(0)),
        });
        let weak = Arc::downgrade(&read);
        let documents = KeyValueDocumentStore {
            source: Source::Retained(read),
            table: "docs".to_owned().into(),
        };
        let control = StorageReadControl::with_limit(4096);
        let mut input = documents
            .begin_max_doc_id_input(if mode == 0 { 16 } else { 17 }, &control)
            .unwrap();
        drop(documents);
        let full = (mode == 2).then(|| {
            control
                .memory()
                .reserve(4096 - control.memory().used())
                .unwrap()
        });
        assert!(input.advance().is_err());
        assert!(input.advance().is_err());
        assert!(weak.upgrade().is_none());
        drop(full);
        assert_eq!(control.memory().used(), 0);
    }
}
