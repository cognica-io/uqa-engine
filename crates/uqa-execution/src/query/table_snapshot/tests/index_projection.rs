//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::memory::Budgeted;

#[derive(Clone)]
struct DecodingSource {
    rows: Arc<MemoryDocumentStore>,
    control: StorageReadControl,
    scratch: Option<usize>,
    headroom: usize,
    visited: Arc<AtomicUsize>,
    cancel_after_stop: bool,
}

impl DocumentStore for DecodingSource {
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("immutable source")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("index reconstruction must use the borrowed projection")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("index reconstruction must page identities")
    }
    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        self.rows.next_doc_ids(after, limit)
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        self.rows.next_doc_ids_controlled(after, limit, control)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.rows.len()
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let bytes = self.scratch.unwrap_or_else(|| {
            self.control.memory().limit() - self.control.memory().used() - self.headroom
        });
        let reservation = self.control.memory().reserve(bytes)?;
        // Model decoded provider workspace that must coexist with its index consumer.
        let _scratch = Budgeted::new(vec![0_u8; bytes], reservation);
        self.rows.for_each_fields_multi_ref_with_presence(
            ids,
            fields,
            &mut |id, present, values| {
                self.visited.fetch_add(1, Ordering::Relaxed);
                let keep_going = visitor(id, present, values);
                if !keep_going && self.cancel_after_stop {
                    self.control.cancellation().cancel();
                }
                keep_going
            },
        )?;
        self.control.check()
    }
}

fn source(
    control: &StorageReadControl,
    scratch: Option<usize>,
    values: &[Value],
) -> DecodingSource {
    let mut rows = MemoryDocumentStore::new();
    for (id, value) in values.iter().enumerate() {
        rows.put_stored(
            u64::try_from(id + 1).unwrap(),
            document(&[("value", value.clone())], 41),
        )
        .unwrap();
    }
    DecodingSource {
        rows: Arc::new(rows),
        control: control.clone(),
        scratch,
        // Leave room for borrowed projection references, but not the reconstructed output.
        headroom: 256,
        visited: Arc::new(AtomicUsize::new(0)),
        cancel_after_stop: false,
    }
}

#[test]
fn provider_projection_allowance_remains_live_during_index_reconstruction() {
    for vector in [false, true] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let columns = columns(if vector {
            "CREATE TABLE t (value VECTOR(1024))"
        } else {
            "CREATE TABLE t (value TEXT)"
        });
        let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        let mut schema = schema(&columns, &index);
        let text_fields = ["value".into()];
        let value = if vector {
            schema.vector_dimensions.insert("value".into(), 1024);
            Value::List(vec![Value::Float(1.0); 1024])
        } else {
            schema.text_fields = &text_fields;
            Value::Str("retained input ".repeat(16))
        };
        let source = source(&control, None, &[value]);
        let visited = Arc::clone(&source.visited);
        let result = retain(
            Arc::new(source),
            &columns,
            &schema,
            DocumentChanges::default(),
            &control,
        );
        let error = result
            .err()
            .expect("provider and consumer share one allowance");
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        assert_eq!(visited.load(Ordering::Relaxed), 1);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn index_reconstruction_stops_at_its_first_error_before_later_provider_cancellation() {
    for headroom in [0, 256] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let columns = columns("CREATE TABLE t (value TEXT)");
        let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        let mut schema = schema(&columns, &index);
        let text_fields = ["value".into()];
        schema.text_fields = &text_fields;
        let mut source = source(
            &control,
            None,
            &[Value::Str("first".into()), Value::Str("second".into())],
        );
        source.headroom = headroom;
        source.cancel_after_stop = true;
        let visited = Arc::clone(&source.visited);
        let result = retain(
            Arc::new(source),
            &columns,
            &schema,
            DocumentChanges::default(),
            &control,
        );
        let error = result.err().expect("consumer cannot exceed the allowance");
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        assert_eq!(visited.load(Ordering::Relaxed), 1);
        assert!(control.cancellation().is_cancelled());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn successful_reconstruction_releases_decode_scratch_and_retains_index_output() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let columns = columns("CREATE TABLE t (value TEXT)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut schema = schema(&columns, &index);
    let text_fields = ["value".into(), "value".into()];
    schema.text_fields = &text_fields;
    let source = source(
        &control,
        Some(16 * 1024),
        &[Value::Str("first".into()), Value::Str("second".into())],
    );
    let visited = Arc::clone(&source.visited);
    let view = retain(
        Arc::new(source),
        &columns,
        &schema,
        DocumentChanges::default(),
        &control,
    )
    .unwrap();
    assert_eq!(visited.load(Ordering::Relaxed), 2);
    assert_eq!(view.document_count, 2);
    assert!(control.memory().used() < 16 * 1024);
    for (id, term) in [(1, "first"), (2, "second")] {
        assert_eq!(
            view.text
                .get_occurrences(id, "value", &uqa_storage::TokenTermKey::from_text(term))
                .unwrap()
                .len(),
            1
        );
    }
    drop(view);
    assert_eq!(control.memory().used(), 0);
}
