//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rebuilt_vectors_keep_the_original_allowance_and_survive_failed_recapture() {
    let columns = columns("CREATE TABLE t (v VECTOR(1024))");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut schema = schema(&columns, &index);
    schema.vector_dimensions.insert("v".into(), 1024);
    for copied in [false, true] {
        let mut source = MemoryDocumentStore::new();
        source
            .put_stored(
                1,
                document(&[("v", Value::List(vec![Value::Float(1.0); 1024]))], 41),
            )
            .unwrap();
        let control = StorageReadControl::with_limit(64 * 1024);
        let capture = |source: &MemoryDocumentStore| {
            if copied {
                materialize(
                    source,
                    &columns,
                    &schema,
                    DocumentChanges::default(),
                    &control,
                )
            } else {
                retain(
                    source.snapshot().unwrap(),
                    &columns,
                    &schema,
                    DocumentChanges::default(),
                    &control,
                )
            }
        };
        let view = capture(&source).unwrap();
        let retained = control.memory().used();
        assert!(retained >= 1024 * size_of::<f32>());
        let full = control
            .memory()
            .reserve(control.memory().limit() - retained)
            .unwrap();
        let error = capture(&source).err().unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        drop(full);
        assert_eq!(control.memory().used(), retained);
        let nested = view.vectors["v"].snapshot().unwrap().snapshot().unwrap();
        let text = view.text.snapshot().unwrap();
        let documents = view.documents.snapshot().unwrap();
        assert_eq!(control.memory().used(), retained);
        source.clear().unwrap();
        drop(view);
        assert_eq!(
            nested
                .search_knn(&[1.0; 1024], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(control.memory().used(), retained);
        drop(nested);
        drop(text);
        drop(documents);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn reconstructed_text_keeps_its_allowance_and_original_occurrences_after_rejection() {
    let columns = columns("CREATE TABLE t (body TEXT)");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut schema = schema(&columns, &index);
    let text_fields = ["body".into()];
    schema.text_fields = &text_fields;
    for copied in [false, true] {
        let mut source = MemoryDocumentStore::new();
        source
            .put_stored(
                1,
                document(&[("body", Value::Str("original ".repeat(512)))], 41),
            )
            .unwrap();
        source
            .put_stored(2, document(&[("body", Value::Str(String::new()))], 42))
            .unwrap();
        source
            .put_stored(3, document(&[("body", Value::Null)], 43))
            .unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let capture = |source: &MemoryDocumentStore| {
            if copied {
                materialize(
                    source,
                    &columns,
                    &schema,
                    DocumentChanges::default(),
                    &control,
                )
            } else {
                retain(
                    source.snapshot().unwrap(),
                    &columns,
                    &schema,
                    DocumentChanges::default(),
                    &control,
                )
            }
        };
        let view = capture(&source).unwrap();
        let retained = control.memory().used();
        assert!(retained >= 512 * size_of::<uqa_core::TokenOccurrence>());
        let key = uqa_storage::TokenTermKey::from_text("original");
        let occurrences = view.text.get_occurrences(1, "body", &key).unwrap();
        assert_eq!(occurrences.len(), 512);
        assert_eq!(view.text.field_doc_count("body").unwrap(), 2);
        assert!(view
            .text
            .indexed_field_metadata(2, "body")
            .unwrap()
            .is_some());
        assert!(view
            .text
            .indexed_field_metadata(3, "body")
            .unwrap()
            .is_none());
        // Leave space for the index header so exhaustion occurs in reconstruction, not admission.
        let full = control
            .memory()
            .reserve(control.memory().limit() - retained - 1024)
            .unwrap();
        let error = capture(&source).err().unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        drop(full);
        assert_eq!(control.memory().used(), retained);
        let nested = view.text.snapshot().unwrap().snapshot().unwrap();
        let documents = view.documents.snapshot().unwrap();
        source.clear().unwrap();
        drop(view);
        assert_eq!(control.memory().used(), retained);
        assert_eq!(
            nested.get_occurrences(1, "body", &key).unwrap(),
            occurrences
        );
        assert_eq!(nested.total_field_length("body").unwrap(), 512);
        drop(nested);
        drop(documents);
        assert_eq!(control.memory().used(), 0);
    }
}

type ReadErrorFactory = fn() -> StorageBackendError;

struct ReadFailure(ReadErrorFactory);

impl DocumentStore for ReadFailure {
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(Self(self.0)))
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
        Err((self.0)())
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(vec![1])
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        control.check()?;
        let mut ids = uqa_core::memory::BudgetedVec::new(control.memory());
        if limit != 0 && after.is_none_or(|after| after < 1) {
            ids.push(1)?;
        }
        Ok(ids)
    }
    fn len(&self) -> StorageBackendResult<usize> {
        Ok(1)
    }
}

#[test]
fn reconstructed_index_reads_preserve_memory_cancellation_and_serialization_sqlstates() {
    let columns = columns("CREATE TABLE t (v VECTOR(2))");
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut schema = schema(&columns, &index);
    schema.vector_dimensions.insert("v".into(), 2);
    let errors: [(ReadErrorFactory, &str); 3] = [
        (
            || uqa_core::memory::MemoryError::SizeOverflow.into(),
            "53200",
        ),
        (|| uqa_core::QueryCancelled.into(), "57014"),
        (
            || {
                uqa_storage::mvcc::VersionError::ReadConflict {
                    dependency: 0,
                    expected: None,
                    actual: None,
                }
                .into_storage_error()
            },
            "40001",
        ),
    ];
    for (error, state) in errors {
        let control = control();
        let error = retain(
            Arc::new(ReadFailure(error)),
            &columns,
            &schema,
            DocumentChanges::default(),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some(state), "{error}");
        assert_eq!(control.memory().used(), 0);
    }
}
