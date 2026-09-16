//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent graph parity, revision ownership, and atomic failure coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::{
    whitespace_analyzer, Analyzer, AnalyzerResources, TokenLengthPolicy, TokenTerm,
};
use uqa_storage::{
    AnalyzerPhase, CatalogFacade, InvertedIndex, KeyValueCatalog, KeyValueInvertedIndex,
    KeyValueStore, MemoryInvertedIndex, MemoryKeyValueStore, RelationIdentity, TableSchema,
    TokenTermKey,
};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn config() -> Analyzer {
    serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"","custom_words":["gap"]},{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#).unwrap()
}

fn assert_same(expected: &dyn InvertedIndex, actual: &dyn InvertedIndex, ids: &[u64]) {
    assert_eq!(actual.doc_count().unwrap(), expected.doc_count().unwrap());
    assert_eq!(
        actual.doc_length_count(None).unwrap(),
        expected.doc_length_count(None).unwrap()
    );
    assert_eq!(
        actual.field_names().unwrap(),
        expected.field_names().unwrap()
    );
    assert_eq!(
        actual.field_doc_count("body").unwrap(),
        expected.field_doc_count("body").unwrap()
    );
    assert_eq!(
        actual.total_field_length("body").unwrap(),
        expected.total_field_length("body").unwrap()
    );
    assert_eq!(
        actual.posting_count(None).unwrap(),
        expected.posting_count(None).unwrap()
    );
    assert_eq!(
        actual.vocabulary_keys("body").unwrap(),
        expected.vocabulary_keys("body").unwrap()
    );
    for id in ids {
        assert_eq!(
            actual.indexed_field_metadata(*id, "body").unwrap(),
            expected.indexed_field_metadata(*id, "body").unwrap()
        );
        assert_eq!(
            actual.get_doc_length(*id, "body").unwrap(),
            expected.get_doc_length(*id, "body").unwrap()
        );
    }
    for key in expected.vocabulary_keys("body").unwrap() {
        assert_eq!(
            actual.get_occurrence_postings("body", &key).unwrap(),
            expected.get_occurrence_postings("body", &key).unwrap()
        );
        assert_eq!(
            actual.doc_freq_key("body", &key).unwrap(),
            expected.doc_freq_key("body", &key).unwrap()
        );
        for id in ids {
            assert_eq!(
                actual.get_occurrences(*id, "body", &key).unwrap(),
                expected.get_occurrences(*id, "body", &key).unwrap()
            );
            assert_eq!(
                actual.get_term_freq_key(*id, "body", &key).unwrap(),
                expected.get_term_freq_key(*id, "body", &key).unwrap()
            );
        }
        let mut left = actual.posting_cursor_key("body", &key).unwrap();
        let mut right = expected.posting_cursor_key("body", &key).unwrap();
        loop {
            assert_eq!(left.current(), right.current());
            if left.current().is_none() {
                break;
            }
            left.advance().unwrap();
            right.advance().unwrap();
        }
    }
}

#[test]
fn persistent_batches_preserve_complete_graphs_across_clusters_and_both_length_policies() {
    for policy in [
        TokenLengthPolicy::EmittedTokens,
        TokenLengthPolicy::DiscountOverlaps,
    ] {
        let revision = AnalyzerResources::default()
            .compile_with_length_policy(&config(), policy)
            .unwrap();
        let store = Arc::new(MemoryKeyValueStore::new());
        let mut actual = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
        let mut expected = MemoryInvertedIndex::new(whitespace_analyzer());
        let ids = [0, 1, 2, 65_535, 65_536, u64::MAX];
        for index in [&mut actual as &mut dyn InvertedIndex, &mut expected] {
            index
                .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
                .unwrap();
            index
                .try_add_documents(vec![
                    (0, fields("a")),
                    (1, fields("gap a gap b gap")),
                    (2, fields("a a")),
                    (65_535, fields("gap gap")),
                    (65_536, fields("a")),
                    (u64::MAX, fields("")),
                ])
                .unwrap();
        }
        assert_same(&expected, &actual, &ids);
        assert_eq!(actual.get_term_freq(1, "body", "a").unwrap(), 3);
        assert_eq!(
            actual
                .get_posting_list("body", "a")
                .unwrap()
                .iter()
                .find(|entry| entry.doc_id == 1)
                .unwrap()
                .payload
                .positions,
            [1]
        );
        assert_eq!(
            actual
                .indexed_field_metadata(1, "body")
                .unwrap()
                .unwrap()
                .final_position_increment,
            1
        );
        for index in [&mut actual as &mut dyn InvertedIndex, &mut expected] {
            index
                .try_add_documents(vec![
                    (1, fields("b b")),
                    (2, fields("b")),
                    (1, fields("gap")),
                    (2, BTreeMap::new()),
                    (u64::MAX, fields("a")),
                ])
                .unwrap();
            index.remove_document(0).unwrap();
            index.remove_document(65_536).unwrap();
        }
        assert_same(&expected, &actual, &ids);
        drop(actual);
        let mut reopened = KeyValueInvertedIndex::new(store, "docs", whitespace_analyzer());
        reopened
            .set_field_analyzer_revisions("body", revision.clone(), revision)
            .unwrap();
        assert_same(&expected, &reopened, &ids);
        let mut cursor = reopened.posting_cursor("body", "a").unwrap();
        cursor.advance_to(u64::MAX).unwrap();
        assert_eq!(cursor.current().unwrap().doc_id, u64::MAX);
        cursor.advance().unwrap();
        assert!(cursor.current().is_none());
        let expected_length = match policy {
            TokenLengthPolicy::EmittedTokens => 3,
            TokenLengthPolicy::DiscountOverlaps => 1,
        };
        assert_eq!(
            reopened
                .get_scoring_inputs_bulk(&[u64::MAX, 1, u64::MAX, 2], "body", &["a".into()])
                .unwrap(),
            vec![
                (expected_length, vec![3]),
                (0, vec![0]),
                (expected_length, vec![3]),
                (0, vec![0])
            ]
        );
        reopened.clear().unwrap();
        assert_eq!(reopened.doc_count().unwrap(), 0);
        assert!(reopened
            .indexed_field_metadata(1, "body")
            .unwrap()
            .is_none());
    }
}

#[test]
fn stored_revision_guards_survive_reopen_and_include_tokenless_fields() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let first = config().compile().unwrap();
    let next = whitespace_analyzer().compile().unwrap();
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
    index
        .set_field_analyzer_revision("body", first.clone(), AnalyzerPhase::Both)
        .unwrap();
    index.add_document(1, fields("gap gap")).unwrap();
    let saved = index.indexed_field_metadata(1, "body").unwrap();
    drop(index);
    let mut index = KeyValueInvertedIndex::new(store, "docs", whitespace_analyzer());
    assert!(index.add_document(2, fields("new")).is_err());
    for phase in [AnalyzerPhase::Index, AnalyzerPhase::Both] {
        assert!(index
            .set_field_analyzer_revision("body", next.clone(), phase)
            .unwrap_err()
            .contains("atomic source rebuild"));
    }
    index
        .set_field_analyzer_revisions("body", first.clone(), next.clone())
        .unwrap();
    assert!(index.remove_field_analyzers("body").is_err());
    assert_eq!(index.indexed_field_metadata(1, "body").unwrap(), saved);
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("body").unwrap(),
        &next
    ));
    index
        .rebuild_with_analyzer_revision(
            "body",
            next.clone(),
            AnalyzerPhase::Index,
            vec![(1, fields("gap gap"))],
        )
        .unwrap();
    assert_eq!(index.get_term_freq(1, "body", "gap").unwrap(), 2);
    assert_eq!(
        index
            .indexed_field_metadata(1, "body")
            .unwrap()
            .unwrap()
            .analyzer_fingerprint,
        next.descriptor().fingerprint()
    );
    index.remove_document(1).unwrap();
    index.remove_field_analyzers("body").unwrap();
}

#[test]
fn failed_analysis_and_storage_publication_preserve_graph_bytes_and_revision_handles() {
    let invalid: Analyzer =
        serde_json::from_str(r#"{"tokenizer":{"type":"n_gram","min_gram":0,"max_gram":1}}"#)
            .unwrap();
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", invalid);
    index
        .set_field_analyzer("body", config(), AnalyzerPhase::Both)
        .unwrap();
    index.add_document(1, fields("gap a gap")).unwrap();
    let before = store.scan_prefix(b"").unwrap();
    let revision = index.index_analyzer_revision("body").unwrap();
    let invalid_fields = BTreeMap::from([
        ("body".into(), "new".into()),
        ("invalid_default".into(), "value".into()),
    ]);
    assert!(index.add_document(1, invalid_fields.clone()).is_err());
    assert!(index
        .try_add_documents(vec![(1, fields("new")), (2, invalid_fields.clone())])
        .is_err());
    assert!(index
        .rebuild_with_analyzer_revision(
            "body",
            whitespace_analyzer().compile().unwrap(),
            AnalyzerPhase::Both,
            vec![(1, fields("new")), (2, invalid_fields)]
        )
        .is_err());
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    store.begin_read_transaction().unwrap();
    assert!(index
        .try_add_documents(vec![(1, fields("replacement")), (2, fields("new"))])
        .unwrap_err()
        .to_string()
        .contains("read-only"));
    assert!(index.remove_document(1).is_err());
    assert!(index
        .rebuild_with_analyzer_revision(
            "body",
            whitespace_analyzer().compile().unwrap(),
            AnalyzerPhase::Both,
            vec![(2, fields("new"))]
        )
        .is_err());
    assert!(!store.transaction_has_written().unwrap());
    store.rollback_transaction().unwrap();
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    assert!(Arc::ptr_eq(
        &index.index_analyzer_revision("body").unwrap(),
        &revision
    ));
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("body").unwrap(),
        &revision
    ));
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 3);
}

#[test]
fn binary_nori_terms_and_source_metadata_follow_column_and_table_lifecycles() {
    let config = match serde_json::from_str::<Analyzer>(
        r#"{"char_filters":[{"type":"html_strip"}],"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"🙂a 가 나"},"token_filters":[]}"#,
    ) {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return;
        }
    };
    let store = Arc::new(MemoryKeyValueStore::new());
    let revision = config.compile().unwrap();
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::from_legacy_name("public.docs").unwrap(),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: serde_json::to_string(&config).unwrap(),
            fts_fields: vec!["body".into()],
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    let mut index = KeyValueInvertedIndex::new(store.clone(), "public.docs", config);
    index.add_document(1, fields("<b>🙂a</b>")).unwrap();
    let raw = TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd83d]));
    let edges = index.get_occurrences(1, "body", &raw).unwrap();
    assert_eq!(edges.len(), 1);
    let offsets = edges[0].offsets.unwrap();
    assert_eq!(
        (
            offsets.start_utf8,
            offsets.end_utf8,
            offsets.start_utf16,
            offsets.end_utf16
        ),
        (3, 7, 4, 5)
    );
    assert_eq!(index.stats().unwrap().doc_freq_utf16("body", &[0xd83d]), 1);
    assert_eq!(index.stats().unwrap().doc_freq("body", "�"), 0);
    assert!(index.vocabulary_terms("body").is_err());
    assert!(index
        .get_occurrences(1, "body", &TokenTermKey::from_text("🙂a"))
        .unwrap()
        .iter()
        .any(|edge| edge.position_length == 2));
    let metadata = index.indexed_field_metadata(1, "body").unwrap();
    catalog
        .rename_column_data("public.docs", "body", "caption")
        .unwrap();
    catalog
        .rename_table_data("public.docs", "public.renamed")
        .unwrap();
    let mut renamed =
        KeyValueInvertedIndex::new(store.clone(), "public.renamed", whitespace_analyzer());
    renamed
        .set_field_analyzer_revision("caption", revision, AnalyzerPhase::Both)
        .unwrap();
    assert_eq!(renamed.get_occurrences(1, "caption", &raw).unwrap(), edges);
    assert_eq!(
        renamed.indexed_field_metadata(1, "caption").unwrap(),
        metadata
    );
    assert!(renamed.indexed_field_metadata(1, "body").unwrap().is_none());
    renamed
        .add_document(2, BTreeMap::from([("caption".into(), "<b>🙂a</b>".into())]))
        .unwrap();
    renamed.remove_document(1).unwrap();
    assert_eq!(renamed.doc_freq_key("caption", &raw).unwrap(), 1);
    catalog
        .drop_column_data("public.renamed", "caption")
        .unwrap();
    assert_eq!(renamed.doc_count().unwrap(), 0);
    assert!(renamed
        .indexed_field_metadata(2, "caption")
        .unwrap()
        .is_none());
    renamed
        .add_document(3, BTreeMap::from([("caption".into(), "🙂a".into())]))
        .unwrap();
    catalog.purge_table_data("public.renamed").unwrap();
    assert_eq!(renamed.doc_count().unwrap(), 0);
    assert!(store.scan_prefix(b"e").unwrap().is_empty());
    renamed
        .add_document(4, BTreeMap::from([("caption".into(), "🙂a".into())]))
        .unwrap();
    catalog.drop_table_and_data("public.renamed").unwrap();
    assert!(store.scan_prefix(b"e").unwrap().is_empty());
}

struct CancellingStore {
    inner: MemoryKeyValueStore,
    cancellation: uqa_core::CancellationToken,
    after_put: std::sync::atomic::AtomicUsize,
    puts: std::sync::atomic::AtomicUsize,
}

struct CancellingBatch<'a> {
    inner: Box<dyn uqa_storage::key_value::KeyValueBatch + 'a>,
    store: &'a CancellingStore,
}

impl uqa_storage::key_value::KeyValueBatch for CancellingBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> uqa_storage::StorageBackendResult<()> {
        self.inner.put(key, value)?;
        let count = self
            .store
            .puts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if count
            == self
                .store
                .after_put
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.store.cancellation.cancel();
        }
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> uqa_storage::StorageBackendResult<()> {
        self.inner.delete(key)
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> uqa_storage::StorageBackendResult<()> {
        self.inner.delete_prefix(prefix)
    }
    fn graph_mutation(
        &mut self,
        mutation: uqa_storage::mvcc::GraphMutation<'_>,
    ) -> uqa_storage::StorageBackendResult<()> {
        self.inner.graph_mutation(mutation)
    }
    fn preview_graph_invalidation(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> uqa_storage::StorageBackendResult<()> {
        self.inner.preview_graph_invalidation(key, value)
    }
    fn replace_graph_cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> uqa_storage::StorageBackendResult<()> {
        self.inner.replace_graph_cache(key, value)
    }
    fn commit(self: Box<Self>) -> uqa_storage::StorageBackendResult<()> {
        self.inner.commit()
    }
}

impl KeyValueStore for CancellingStore {
    fn get(&self, key: &[u8]) -> uqa_storage::StorageBackendResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> uqa_storage::StorageBackendResult<()> {
        self.inner.put(key, value)
    }
    fn delete(&self, key: &[u8]) -> uqa_storage::StorageBackendResult<()> {
        self.inner.delete(key)
    }
    fn scan_prefix(
        &self,
        prefix: &[u8],
    ) -> uqa_storage::StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan_prefix(prefix)
    }
    fn delete_prefix(&self, prefix: &[u8]) -> uqa_storage::StorageBackendResult<usize> {
        self.inner.delete_prefix(prefix)
    }
    fn batch(&self) -> Box<dyn uqa_storage::key_value::KeyValueBatch + '_> {
        Box::new(CancellingBatch {
            inner: self.inner.batch(),
            store: self,
        })
    }
    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }
    fn transaction_has_written(&self) -> uqa_storage::StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }
    fn visit_value(
        &self,
        key: &[u8],
        control: &uqa_storage::read_control::StorageReadControl,
        visit: &mut uqa_storage::read_control::ValueReadVisitor<'_>,
    ) -> uqa_storage::StorageBackendResult<()> {
        self.inner.visit_value(key, control, visit)
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &uqa_storage::read_control::StorageReadControl,
        visit: &mut uqa_storage::read_control::KeyValueReadVisitor<'_>,
    ) -> uqa_storage::StorageBackendResult<()> {
        self.inner
            .visit_prefix_after(prefix, after, limit, control, visit)
    }
}

#[test]
fn cancelled_rebuild_discards_buffered_writes_and_preserves_both_revisions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let cancellation = uqa_core::CancellationToken::new();
    let store = Arc::new(CancellingStore {
        inner: MemoryKeyValueStore::new(),
        cancellation: cancellation.clone(),
        after_put: AtomicUsize::new(usize::MAX),
        puts: AtomicUsize::new(0),
    });
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", config());
    let mut expected = MemoryInvertedIndex::new(config());
    for (id, text) in [(1, "a gap old"), (2, "old a")] {
        index.add_document(id, fields(text)).unwrap();
        expected.add_document(id, fields(text)).unwrap();
    }
    let before = store.scan_prefix(b"").unwrap();
    let index_revision = index.index_analyzer_revision("body").unwrap();
    let search_revision = index.search_analyzer_revision("body").unwrap();
    let next = uqa_analysis::keyword_analyzer().compile().unwrap();
    let documents = || {
        vec![
            (3, fields("new one")),
            (4, fields("new two")),
            (5, fields("new three")),
        ]
    };
    for target in [1, 2, 4] {
        store.puts.store(0, Ordering::Relaxed);
        store.after_put.store(target, Ordering::Relaxed);
        let error = index
            .rebuild_with_analyzer_revision_cancellable(
                "body",
                next.clone(),
                AnalyzerPhase::Both,
                documents(),
                &cancellation,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            uqa_storage::StorageBackendError::Cancelled(_)
        ));
        assert!(store.puts.load(Ordering::Relaxed) >= target);
        assert_eq!(store.scan_prefix(b"").unwrap(), before);
        assert!(Arc::ptr_eq(
            &index_revision,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &search_revision,
            &index.search_analyzer_revision("body").unwrap()
        ));
        cancellation.reset();
        assert_same(&expected, &index, &[1, 2, 3, 4, 5]);
    }
    store.after_put.store(usize::MAX, Ordering::Relaxed);
    index
        .rebuild_with_analyzer_revision_cancellable(
            "body",
            next,
            AnalyzerPhase::Both,
            documents(),
            &cancellation,
        )
        .unwrap();
    let mut recovered = MemoryInvertedIndex::new(uqa_analysis::keyword_analyzer());
    recovered.try_rebuild_documents(documents()).unwrap();
    assert_same(&recovered, &index, &[1, 2, 3, 4, 5]);
}

#[test]
fn cancelled_memory_rebuild_retains_graphs_metadata_and_revisions_until_recovery() {
    for change_revision in [false, true] {
        let documents = vec![(1, fields("gap a b")), (2, fields("a gap"))];
        let mut index = MemoryInvertedIndex::new(config());
        let mut expected = MemoryInvertedIndex::new(config());
        index.try_rebuild_documents(documents.clone()).unwrap();
        expected.try_rebuild_documents(documents).unwrap();
        let before_index = index.index_analyzer_revision("body").unwrap();
        let before_search = index.search_analyzer_revision("body").unwrap();
        let next = uqa_analysis::keyword_analyzer().compile().unwrap();
        let cancellation = uqa_core::CancellationToken::new();
        cancellation.cancel();
        let replacement = vec![(3, fields("new whole")), (4, fields("한국어 😀"))];
        let error = if change_revision {
            index.rebuild_with_analyzer_revision_cancellable(
                "body",
                next.clone(),
                AnalyzerPhase::Both,
                replacement.clone(),
                &cancellation,
            )
        } else {
            index.try_rebuild_documents_cancellable(replacement.clone(), &cancellation)
        }
        .unwrap_err();
        assert!(matches!(
            error,
            uqa_storage::StorageBackendError::Cancelled(_)
        ));
        assert!(Arc::ptr_eq(
            &before_index,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &before_search,
            &index.search_analyzer_revision("body").unwrap()
        ));
        assert_same(&expected, &index, &[1, 2, 3, 4]);
        cancellation.reset();
        index
            .rebuild_with_analyzer_revision_cancellable(
                "body",
                next,
                AnalyzerPhase::Both,
                replacement.clone(),
                &cancellation,
            )
            .unwrap();
        let mut recovered = MemoryInvertedIndex::new(uqa_analysis::keyword_analyzer());
        recovered.try_rebuild_documents(replacement).unwrap();
        assert_same(&recovered, &index, &[1, 2, 3, 4]);
    }
}
