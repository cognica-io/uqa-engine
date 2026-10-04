//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unchanged complete postings avoid physical replacement while preserving source effects.

use super::*;
use crate::key_value::{KeyValueInvertedIndex, KeyValueStore, MemoryKeyValueStore};
use crate::InvertedIndex;
use std::sync::Arc;

struct RecordedBatch<'a> {
    inner: &'a mut dyn KeyValueBatch,
    replacements: Vec<Vec<u8>>,
    guards: Vec<DocId>,
}

impl KeyValueBatch for RecordedBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.inner.put(key, value)
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.inner.delete(key)
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.inner.delete_prefix(prefix)
    }
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.replacements.push(key.to_vec());
        self.inner.replace_occurrence_record(key, value)
    }
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.inner.invalidate_occurrence_prefix(prefix)
    }
    fn occurrence_document(&mut self, table: &str, doc_id: DocId) -> StorageBackendResult<()> {
        self.guards.push(doc_id);
        self.inner.occurrence_document(table, doc_id)
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        panic!("the enclosing mutation scope owns commit")
    }
}

fn fields(text: &str) -> BTreeMap<FieldName, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn replace(index: &KeyValueInvertedIndex, text: &str) -> usize {
    let mut cluster_writes = 0;
    index
        .mutate(|view, batch| {
            let mut recorded = RecordedBatch {
                inner: batch,
                replacements: Vec::new(),
                guards: Vec::new(),
            };
            let mut observations = 0;
            view.add_documents(
                &mut recorded,
                vec![(17, fields(text))],
                Some(&mut |_| {
                    observations += 1;
                    Ok(())
                }),
            )?;
            assert!(observations > 0, "logical text writes remain observable");
            assert_eq!(recorded.guards, [17]);
            let score = keys::kind_prefix("docs", keys::SCORE)?;
            let positions = keys::kind_prefix("docs", keys::POSITIONS)?;
            cluster_writes = recorded
                .replacements
                .iter()
                .filter(|key| key.starts_with(&score) || key.starts_with(&positions))
                .count();
            assert!(recorded
                .replacements
                .contains(&keys::kind_prefix("docs", keys::FORMAT)?));
            assert!(recorded.replacements.contains(&keys::field_prefix(
                "docs",
                keys::FIELD,
                "body"
            )?));
            Ok(())
        })
        .unwrap();
    cluster_writes
}

#[test]
fn unchanged_clusters_keep_metadata_observations_and_complete_occurrence_comparisons() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let mut index =
        KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
    index
        .try_add_documents(vec![
            (17, fields("alpha beta alpha")),
            (42, fields("alpha beta")),
        ])
        .unwrap();
    let original = store.scan_prefix(b"").unwrap();
    assert_eq!(replace(&index, "alpha beta alpha"), 0);
    assert_eq!(store.scan_prefix(b"").unwrap(), original);
    let before = index.indexed_field_metadata(17, "body").unwrap().unwrap();
    assert_eq!(replace(&index, "alpha beta alpha   "), 0);
    let after = index.indexed_field_metadata(17, "body").unwrap().unwrap();
    assert_eq!(after.length, before.length);
    assert_eq!(
        after.final_offsets.end_utf8,
        before.final_offsets.end_utf8 + 3
    );
    // Equal frequencies and lengths do not establish equality of source offsets.
    assert_eq!(replace(&index, " alpha beta alpha"), 4);
    // Equal term support does not establish equality of positional graphs.
    assert_eq!(replace(&index, "beta alpha alpha"), 4);
    for (term, expected) in [("beta", vec![(0, 0)]), ("alpha", vec![(1, 5), (2, 11)])] {
        let occurrences = index
            .get_occurrences(17, "body", &TokenTermKey::from_text(term))
            .unwrap();
        assert_eq!(
            occurrences
                .iter()
                .map(|occurrence| (occurrence.position, occurrence.offsets.unwrap().start_utf8))
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(replace(&index, "beta alpha alpha alpha"), 4);
    assert_eq!(index.get_doc_length(17, "body").unwrap(), 4);
    assert_eq!(index.get_term_freq(17, "body", "alpha").unwrap(), 3);
    assert_eq!(index.get_doc_length(42, "body").unwrap(), 2);
    assert_eq!(index.doc_count().unwrap(), 2);
    assert_eq!(index.total_field_length("body").unwrap(), 6);
}

#[test]
fn identical_replacements_still_reject_corrupt_occurrence_payloads_atomically() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let mut index =
        KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
    index.add_document(17, fields("alpha")).unwrap();
    let key = keys::cluster_key(
        "docs",
        keys::POSITIONS,
        "body",
        &TokenTermKey::from_text("alpha"),
        cluster_id(17),
    )
    .unwrap();
    store.put(&key, b"invalid graph").unwrap();
    let corrupted = store.scan_prefix(b"").unwrap();
    assert!(index.add_document(17, fields("alpha")).is_err());
    assert_eq!(store.scan_prefix(b"").unwrap(), corrupted);
}
