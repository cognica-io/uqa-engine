//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic interleavings at the occurrence reader and evaluated-batch boundaries.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_storage::key_value::{
    KeyValueMutation, KeyValueRead, KeyValueReadRevision, KeyValueReadScope,
};
use uqa_storage::read_control::{KeyValueReadVisitor, ValueReadVisitor};
use uqa_storage::{
    InvertedIndex, KeyValueBatch, KeyValueInvertedIndex, StorageBackendResult, TokenTermKey,
};

type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;
struct InterleavedStore {
    inner: Arc<VersionedKeyValueStore>,
    point_reads: AtomicUsize,
    evaluations: AtomicUsize,
    after_second_point: Hook,
    after_evaluation: Hook,
}
impl InterleavedStore {
    fn new(inner: Arc<VersionedKeyValueStore>) -> Self {
        Self {
            inner,
            point_reads: AtomicUsize::new(0),
            evaluations: AtomicUsize::new(0),
            after_second_point: Mutex::new(None),
            after_evaluation: Mutex::new(None),
        }
    }
    fn point_read(&self) {
        if self.point_reads.fetch_add(1, Ordering::Relaxed) == 1 {
            fire(&self.after_second_point);
        }
    }
}
fn fire(hook: &Hook) {
    let action = hook.lock().take();
    if let Some(action) = action {
        action();
    }
}
struct InterleavedRead<'a> {
    inner: &'a dyn KeyValueRead,
    store: &'a InterleavedStore,
}
impl KeyValueRead for InterleavedRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.inner.control()
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.inner.revision(prefixes)
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_value(key, visit)?;
        self.store.point_read();
        Ok(())
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_prefix(prefix, visit)
    }
    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner.visit_value_budgeted(key, control, visit)?;
        self.store.point_read();
        Ok(())
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.inner
            .visit_prefix_after(prefix, after, limit, control, visit)
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.inner.contains_prefix_budgeted(prefix, control)
    }
}
impl KeyValueStore for InterleavedStore {
    fn with_read_view(&self, visit: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        self.inner.with_read_view(&mut |read| {
            visit(&InterleavedRead {
                inner: read,
                store: self,
            })
        })
    }
    fn with_mutation(&self, mutate: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        self.inner.with_mutation(&mut |read, batch| {
            self.evaluations.fetch_add(1, Ordering::Relaxed);
            mutate(
                &InterleavedRead {
                    inner: read,
                    store: self,
                },
                batch,
            )?;
            fire(&self.after_evaluation);
            Ok(())
        })
    }
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        let result = self.inner.get(key)?;
        self.point_read();
        Ok(result)
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
        fire(&self.after_evaluation);
        self.inner.batch()
    }
    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

#[test]
fn compound_occurrence_queries_keep_original_rows_during_an_intervening_commit() {
    for query in 0..4 {
        let persistence = Persistence::new();
        let a = Arc::new(InterleavedStore::new(Arc::new(
            persistence.session(1 << 20),
        )));
        let b = Arc::new(persistence.session(1 << 20));
        let mut writer = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
        writer.add_document(1, fields("alpha alpha beta")).unwrap();
        *a.after_second_point.lock() = Some(Box::new(move || {
            writer
                .add_document(1, fields("gamma gamma gamma gamma"))
                .unwrap();
        }));
        let index =
            KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
        let term = TokenTermKey::from_text("alpha");
        let control = StorageReadControl::with_limit(1 << 20);
        match query {
            0 => assert_eq!(index.get_doc_length(1, "body").unwrap(), 3),
            1 => assert_eq!(
                index
                    .get_scoring_inputs_keys_bulk(&[1, 1], "body", &[term.clone(), term.clone()])
                    .unwrap(),
                vec![(3, vec![2, 2]); 2]
            ),
            2 => assert_eq!(
                index
                    .get_occurrences_budgeted(1, "body", &term, &control)
                    .unwrap()
                    .len(),
                2
            ),
            _ => assert_eq!(
                index
                    .get_posting_lists_keys_bulk("body", &[term.clone(), term])
                    .unwrap()
                    .iter()
                    .map(uqa_core::PostingList::len)
                    .collect::<Vec<_>>(),
                vec![1, 1]
            ),
        }
        assert!(
            a.after_second_point.lock().is_none(),
            "schedule must commit during the read"
        );
        assert_eq!(index.get_doc_length(1, "body").unwrap(), 4);
        assert_eq!(index.doc_freq("body", "alpha").unwrap(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn total_term_frequency_retains_one_view_across_fields() {
    let persistence = Persistence::new();
    let a = Arc::new(InterleavedStore::new(Arc::new(
        persistence.session(1 << 20),
    )));
    let b = Arc::new(persistence.session(1 << 20));
    let mut writer = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    let fields = |left: &str, right: &str| {
        BTreeMap::from([("a".into(), left.into()), ("body".into(), right.into())])
    };
    writer
        .add_document(1, fields("alpha", "alpha alpha"))
        .unwrap();
    *a.after_second_point.lock() = Some(Box::new(move || {
        writer
            .add_document(
                1,
                fields("alpha alpha alpha alpha", "alpha alpha alpha alpha"),
            )
            .unwrap();
    }));
    let index = KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    assert_eq!(index.get_total_term_freq(1, "alpha").unwrap(), 3);
    assert!(a.after_second_point.lock().is_none());
    assert_eq!(index.get_total_term_freq(1, "alpha").unwrap(), 8);
}

#[test]
fn controlled_occurrence_cursors_keep_their_view_between_cluster_pages() {
    use uqa_storage::clustered_postings::PostingReadCursor;
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 20));
    let b = Arc::new(persistence.session(1 << 20));
    let mut writer = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    writer
        .try_add_documents(vec![
            (1, fields("alpha")),
            (65_536, fields("alpha alpha")),
            (131_072, fields("alpha alpha alpha")),
        ])
        .unwrap();
    let reader = KeyValueInvertedIndex::new(a, "docs", uqa_analysis::whitespace_analyzer());
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let mut cursor = reader
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    writer
        .try_rebuild_documents(vec![
            (1, fields("alpha")),
            (65_536, fields("alpha alpha alpha alpha")),
        ])
        .unwrap();
    assert_eq!(cursor.doc_freq(), 3);
    assert_eq!(cursor.advance().unwrap().unwrap().term_freq, 2);
    assert_eq!(cursor.advance_to(131_072).unwrap().unwrap().term_freq, 3);
    assert_eq!(cursor.advance().unwrap(), None);
    drop(cursor);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(reader.doc_count().unwrap(), 2);
}

#[test]
fn occurrence_evaluation_retains_original_preconditions_and_never_replays() {
    let persistence = Persistence::new();
    let a = Arc::new(InterleavedStore::new(Arc::new(
        persistence.session(1 << 20),
    )));
    let b = Arc::new(persistence.session(1 << 20));
    let mut winner =
        KeyValueInvertedIndex::new(b.clone(), "docs", uqa_analysis::whitespace_analyzer());
    winner.add_document(1, fields("alpha")).unwrap();
    *a.after_evaluation.lock() = Some(Box::new(move || {
        winner.add_document(3, fields("alpha alpha alpha")).unwrap();
    }));
    let mut loser =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    assert!(loser.add_document(2, fields("alpha alpha")).is_err());
    assert_eq!(a.evaluations.load(Ordering::Relaxed), 1);
    assert!(a.inner.commit_transaction().is_err());
    assert_eq!(a.evaluations.load(Ordering::Relaxed), 1);
    let committed = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    assert_eq!(committed.doc_count().unwrap(), 2);
    assert_eq!(committed.total_field_length("body").unwrap(), 4);
    assert_eq!(committed.get_term_freq(2, "body", "alpha").unwrap(), 0);
    assert_eq!(committed.get_term_freq(3, "body", "alpha").unwrap(), 3);
    a.inner.rollback_transaction().unwrap();
    assert_eq!(loser.total_field_length("body").unwrap(), 4);
}

#[test]
fn occurrence_snapshots_share_allowance_and_release_failed_capture() {
    let persistence = Persistence::new();
    let writer = Arc::new(persistence.session(1 << 20));
    let mut index =
        KeyValueInvertedIndex::new(writer.clone(), "docs", uqa_analysis::whitespace_analyzer());
    index
        .try_add_documents((0..64).map(|id| (id, fields("alpha alpha beta"))).collect())
        .unwrap();
    let limited = Arc::new(persistence.session(4096));
    let bounded =
        KeyValueInvertedIndex::new(limited.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let lightweight = bounded.snapshot().unwrap();
    assert!(limited.retention_control().memory().used() < 4096);
    assert_eq!(
        lightweight.field_stats_scalar("body").unwrap().total_docs,
        64
    );
    drop(lightweight);
    assert_eq!(limited.retention_control().memory().used(), 0);
    let hold = limited.retention_control().memory().reserve(4096).unwrap();
    assert!(matches!(
        bounded.snapshot(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(limited.retention_control().memory().used(), 4096);
    drop(hold);
    assert_eq!(limited.retention_control().memory().used(), 0);
    let control = writer.retention_control();
    let snapshot = index.snapshot().unwrap();
    let charged = control.memory().used();
    assert!(charged > 0 && charged < 4096);
    let nested = snapshot.snapshot().unwrap();
    assert_eq!(control.memory().used(), charged);
    drop(index);
    drop(snapshot);
    assert_eq!(nested.doc_count().unwrap(), 64);
    assert_eq!(control.memory().used(), charged);
    control.cancellation().cancel();
    assert!(matches!(
        nested.doc_count(),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}
