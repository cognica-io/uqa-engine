//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
struct CancelOnReturn {
    rows: Arc<dyn DocumentStore>,
    cancellation: CancellationToken,
    armed: Arc<AtomicBool>,
    conflict: bool,
}

impl CancelOnReturn {
    fn finish<T>(&self, result: StorageBackendResult<T>) -> StorageBackendResult<T> {
        assert!(
            !self.cancellation.is_cancelled(),
            "a retained read continued into another provider call after cancellation"
        );
        if self.armed.swap(false, Ordering::Relaxed) {
            self.cancellation.cancel();
            if self.conflict {
                return Err(uqa_storage::mvcc::VersionError::ReadConflict {
                    dependency: 7,
                    expected: None,
                    actual: None,
                }
                .into_storage_error());
            }
        }
        result
    }
}

impl DocumentStore for CancelOnReturn {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable read probe")
    }

    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable read probe")
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable read probe")
    }

    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.finish(self.rows.get_stored(id))
    }

    fn get_stored_many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        self.finish(self.rows.get_stored_many(ids))
    }

    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        self.finish(self.rows.get_field(id, field))
    }

    fn get_metadata(&self, id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.finish(self.rows.get_metadata(id))
    }

    fn contains_doc_id(&self, id: DocId) -> StorageBackendResult<bool> {
        self.finish(self.rows.contains_doc_id(id))
    }

    fn get_shared_fields(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<uqa_storage::SharedDocumentRow>>>> {
        self.finish(self.rows.get_shared_fields(ids, fields))
    }

    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::SharedDocumentRow)>>> {
        self.finish(self.rows.next_shared_fields(after, limit, fields))
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.rows.doc_ids()
    }

    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}

type Read = fn(&dyn DocumentStore, DocId) -> StorageBackendResult<()>;

fn check_return_boundary(conflict: bool) {
    for private in [false, true] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut rows = MemoryDocumentStore::new();
        rows.put_stored(1, document(&[("key", Value::Null)], 41))
            .unwrap();
        let source = CancelOnReturn {
            rows: rows.snapshot().unwrap(),
            cancellation: control.cancellation().clone(),
            armed: Arc::default(),
            conflict,
        };
        let mut columns = columns("CREATE TABLE t (key INTEGER)");
        columns[0].missing_value = Some(Value::Int(17));
        let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        let (base, changes): (Arc<dyn DocumentStore>, _) = if private {
            (
                Arc::new(MemoryDocumentStore::new()),
                DocumentChanges::default()
                    .with_retained(source.snapshot().unwrap(), selection([(1, true)]), &control)
                    .unwrap(),
            )
        } else {
            (source.snapshot().unwrap(), DocumentChanges::default())
        };
        let view = retain(base, &columns, &schema(&columns, &index), changes, &control).unwrap();
        let nested = view.documents.snapshot().unwrap().snapshot().unwrap();
        let retained = control.memory().used();
        let reads: [(&str, Read); 8] = [
            ("document", |rows, id| rows.get_stored(id).map(|_| ())),
            ("field", |rows, id| rows.get_field(id, "key").map(|_| ())),
            ("unknown field", |rows, id| {
                rows.get_field(id, "absent").map(|_| ())
            }),
            ("metadata", |rows, id| rows.get_metadata(id).map(|_| ())),
            ("bulk document", |rows, id| {
                rows.get_stored_many(&[id]).map(|_| ())
            }),
            ("presence", |rows, id| rows.contains_doc_id(id).map(|_| ())),
            ("shared projection", |rows, id| {
                rows.get_shared_fields(&[id], &["key"]).map(|_| ())
            }),
            ("shared page", |rows, id| {
                rows.next_shared_fields(id.checked_sub(1), 1, &["key"])
                    .map(|_| ())
            }),
        ];
        for documents in [view.documents.as_ref(), nested.as_ref()] {
            let selected = if private { &reads[..5] } else { &reads[..] };
            let ids: &[DocId] = if private { &[1] } else { &[1, 99] };
            for (name, read) in selected {
                for id in ids {
                    control.cancellation().reset();
                    source.armed.store(true, Ordering::Relaxed);
                    let error = read(documents, *id).expect_err(name);
                    assert!(control.cancellation().is_cancelled(), "{name}, {id}");
                    assert_eq!(
                        snapshot_error("selected read", &error).sqlstate(),
                        Some(if conflict { "40001" } else { "57014" }),
                        "{name}, {id}, private={private}: {error}"
                    );
                    assert_eq!(control.memory().used(), retained, "{name}, {id}");
                    control.cancellation().reset();
                    read(documents, *id).unwrap();
                }
            }
            assert_eq!(documents.get_field(1, "key").unwrap(), Some(Value::Null));
            assert_eq!(
                documents.get_metadata(1).unwrap().unwrap().tuple_xmin(),
                Some(41)
            );
            assert_eq!(documents.len().unwrap(), 1);
            assert_eq!(control.memory().used(), retained);
        }
        if !private {
            assert!(view
                .documents
                .get_shared_fields(&[1], &["key"])
                .unwrap()
                .is_none());
            assert!(view
                .documents
                .next_shared_fields(None, 1, &["key"])
                .unwrap()
                .is_none());
        }
        drop(view);
        assert!(control.memory().used() > 0);
        assert!(control.memory().used() <= retained);
        drop(nested);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn retained_reads_reject_cancellation_during_provider_return() {
    check_return_boundary(false);
}

#[test]
fn retained_reads_preserve_provider_failure_before_later_cancellation() {
    check_return_boundary(true);
}
