//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::Value;

use crate::clustered_postings::decode_terms;
use crate::document_store::DocumentStore;
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::{
    HNSWIndexParams, IVFIndexParams, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};

use super::codec::*;
use super::{
    KeyValueCatalog, KeyValueDocumentStore, KeyValueInvertedIndex, KeyValueStore,
    KeyValueVectorIndex, MemoryKeyValueStore, DOCUMENT_VALUE_V1_PREFIX, TAG_METADATA,
};
use crate::{PersistentStorageBackend, StorageBackendError};

mod indexes;

#[test]
fn key_segment_length_rejects_values_outside_the_disk_format() {
    let max_u32 = usize::try_from(u32::MAX).unwrap();
    assert_eq!(key_segment_length(max_u32).unwrap(), u32::MAX);
    if usize::BITS > u32::BITS {
        let error = key_segment_length(max_u32 + 1).unwrap_err();
        assert!(error.to_string().contains("u32 on-disk format"));
    }
}

#[test]
fn key_readers_reject_offset_overflow() {
    let mut offset = usize::MAX;
    let error = read_u64(&[], &mut offset).unwrap_err();
    assert!(error.to_string().contains("offset overflow"));
    assert_eq!(offset, usize::MAX);
}

#[test]
fn vector_ordinal_count_matches_zero_based_u32_format() {
    validate_vector_ordinal_count(u64::from(u32::MAX) + 1).unwrap();
    let error = validate_vector_ordinal_count(u64::from(u32::MAX) + 2).unwrap_err();
    assert!(error.to_string().contains("u32 index format"));
}
use crate::catalog::{CatalogFacade, TableSchema};
use uqa_analysis::{standard_analyzer, Analyzer, Tokenizer};

fn store() -> Arc<dyn KeyValueStore> {
    Arc::new(MemoryKeyValueStore::new())
}

#[test]
fn memory_store_passes_the_reusable_backend_contract() {
    super::conformance::verify_store(&MemoryKeyValueStore::new()).unwrap();
}

#[test]
fn memory_key_value_scan_and_batch_are_ordered_and_atomic() {
    let store = store();
    store.put(b"p/a/2", b"two").unwrap();
    store.put(b"p/a/1", b"one").unwrap();
    store.put(b"p/b/1", b"other").unwrap();
    let rows = store.scan_prefix(b"p/a/").unwrap();
    assert_eq!(
        rows,
        vec![
            (b"p/a/1".to_vec(), b"one".to_vec()),
            (b"p/a/2".to_vec(), b"two".to_vec())
        ]
    );
    assert_eq!(
        store
            .scan_prefix_keys_after(b"p/a/", Some(b"p/a/1"), 1)
            .unwrap(),
        vec![b"p/a/2".to_vec()]
    );
    assert_eq!(
        store
            .scan_prefix_keys_after(b"p/a/", Some(b"a"), 2)
            .unwrap(),
        vec![b"p/a/1".to_vec(), b"p/a/2".to_vec()]
    );
    assert!(store
        .scan_prefix_keys_after(b"p/a/", Some(b"z"), 2)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.first_prefix_after(b"p/a/", Some(b"a")).unwrap(),
        Some((b"p/a/1".to_vec(), b"one".to_vec()))
    );
    assert!(store
        .scan_prefix_keys_after(b"p/a/", None, 0)
        .unwrap()
        .is_empty());

    let mut batch = store.batch();
    batch.delete(b"p/a/1").unwrap();
    batch.put(b"p/a/3", b"three").unwrap();
    batch.commit().unwrap();
    let rows = store.scan_prefix(b"p/a/").unwrap();
    assert_eq!(
        rows,
        vec![
            (b"p/a/2".to_vec(), b"two".to_vec()),
            (b"p/a/3".to_vec(), b"three".to_vec())
        ]
    );
}

#[test]
fn key_value_backend_persists_btree_definitions_and_incremental_values() {
    let store = store();
    let backend = super::KeyValueStorageBackend::new(Arc::clone(&store));
    backend
        .replace_btree_index("items", "price", &[(1, Value::Int(10)), (2, Value::Null)])
        .unwrap();
    assert_eq!(backend.btree_index_fields("items").unwrap(), vec!["price"]);
    assert_eq!(
        backend.load_btree_index("items", "price").unwrap(),
        Some(vec![(1, Value::Int(10)), (2, Value::Null)])
    );

    backend
        .apply_btree_index_write(
            "items",
            2,
            Some(&BTreeMap::from([("price".into(), Value::Int(25))])),
        )
        .unwrap();
    backend.apply_btree_index_write("items", 1, None).unwrap();
    assert_eq!(
        backend.load_btree_index("items", "price").unwrap(),
        Some(vec![(2, Value::Int(25))])
    );

    backend.clear_btree_indexes("items").unwrap();
    assert_eq!(
        backend.load_btree_index("items", "price").unwrap(),
        Some(Vec::new())
    );
    backend.drop_btree_index("items", "price").unwrap();
    assert_eq!(backend.load_btree_index("items", "price").unwrap(), None);
}

#[test]
fn key_value_ivf_restores_physical_state_and_mutations() {
    let store = store();
    let backend = super::KeyValueStorageBackend::new(Arc::clone(&store));
    let params = IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 2,
    };
    let mut index = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::IVF(params),
            VectorIndexOpenMode::Create,
        )
        .unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.initialize().unwrap();
    assert_eq!(index.index_kind(), "ivf");
    drop(index);

    let mut restored = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::IVF(params),
            VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(ids(&restored.search_knn(&[1.0, 0.0], 1).unwrap()), vec![1]);
    restored.add(3, vec![0.9, 0.1]).unwrap();
    restored.delete(1).unwrap();
    drop(restored);

    let restored = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::IVF(params),
            VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(restored.count().unwrap(), 2);
    assert_eq!(ids(&restored.search_knn(&[1.0, 0.0], 1).unwrap()), vec![3]);
}

#[test]
fn key_value_hnsw_restores_graph_and_incremental_deltas() {
    let store = store();
    let backend = super::KeyValueStorageBackend::new(Arc::clone(&store));
    let params = HNSWIndexParams {
        m: 4,
        ef_construction: 16,
        ef_search: 16,
        rebuild_threshold: 8,
        seed: 7,
    };
    let mut index = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::HNSW(params),
            VectorIndexOpenMode::Create,
        )
        .unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.initialize().unwrap();
    assert_eq!(index.index_kind(), "hnsw");
    drop(index);

    let mut restored = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::HNSW(params),
            VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(ids(&restored.search_knn(&[0.0, 1.0], 1).unwrap()), vec![2]);
    restored.add(3, vec![0.1, 0.9]).unwrap();
    restored.delete(2).unwrap();
    drop(restored);

    let restored = backend
        .vector_index(
            "items",
            "embedding",
            2,
            VectorIndexSpec::HNSW(params),
            VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(restored.count().unwrap(), 2);
    assert_eq!(ids(&restored.search_knn(&[0.0, 1.0], 1).unwrap()), vec![3]);
}

fn ids(postings: &uqa_core::PostingList) -> Vec<uqa_core::DocId> {
    postings.iter().map(|entry| entry.doc_id).collect()
}

#[test]
fn key_value_document_cursor_returns_bounded_ordered_pages() {
    let mut docs = KeyValueDocumentStore::new(store(), "articles");
    for doc_id in [8, 2, 13, 5] {
        docs.put(
            doc_id,
            BTreeMap::from([("id".to_string(), Value::Int(doc_id as i64))]),
        )
        .unwrap();
    }

    assert_eq!(docs.next_doc_ids(None, 2).unwrap(), vec![2, 5]);
    assert_eq!(docs.next_doc_ids(Some(5), 2).unwrap(), vec![8, 13]);
    assert!(docs.next_doc_ids(Some(13), 2).unwrap().is_empty());
    assert!(docs.next_doc_ids(None, 0).unwrap().is_empty());
}

#[test]
fn key_value_document_store_round_trips_documents() {
    let mut docs = KeyValueDocumentStore::new(store(), "articles");
    docs.put(
        7,
        BTreeMap::from([
            ("title".to_string(), Value::Str("Rust".into())),
            ("body".to_string(), Value::Bytes(vec![1, 2, 3])),
            (
                "numbers".to_string(),
                Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
            ),
        ]),
    )
    .unwrap();
    assert_eq!(docs.doc_ids().unwrap(), vec![7]);
    assert_eq!(
        docs.get_field(7, "title").unwrap(),
        Some(Value::Str("Rust".into()))
    );
    assert_eq!(
        docs.get_field(7, "body").unwrap(),
        Some(Value::Bytes(vec![1, 2, 3]))
    );
    assert_eq!(
        docs.get_field(7, "numbers").unwrap(),
        Some(Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))
    );
}

#[test]
fn key_value_document_codec_preserves_legacy_and_new_array_meanings() {
    let store = store();
    store
        .put(
            &document_key("articles", 7).unwrap(),
            br#"{"legacy_bytes":[1,2],"legacy_empty":[],"legacy_list":[1,300],"nested":[[3,4]]}"#,
        )
        .unwrap();

    let mut docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
    docs.put(
        8,
        BTreeMap::from([
            (
                "new_list".into(),
                Value::List(vec![Value::Int(1), Value::Int(2)]),
            ),
            ("new_empty".into(), Value::List(Vec::new())),
            ("new_bytes".into(), Value::Bytes(vec![1, 2])),
        ]),
    )
    .unwrap();
    drop(docs);

    let docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
    let legacy = docs.get(7).unwrap().unwrap();
    assert_eq!(legacy["legacy_bytes"], Value::Bytes(vec![1, 2]));
    assert_eq!(legacy["legacy_empty"], Value::Bytes(Vec::new()));
    assert_eq!(
        legacy["legacy_list"],
        Value::List(vec![Value::Int(1), Value::Int(300)])
    );
    assert_eq!(
        legacy["nested"],
        Value::List(vec![Value::Bytes(vec![3, 4])])
    );
    let current = docs.get(8).unwrap().unwrap();
    assert_eq!(
        current["new_list"],
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(current["new_empty"], Value::List(Vec::new()));
    assert_eq!(current["new_bytes"], Value::Bytes(vec![1, 2]));
    assert!(store
        .get(&document_key("articles", 8).unwrap())
        .unwrap()
        .unwrap()
        .starts_with(DOCUMENT_VALUE_V1_PREFIX));
}

#[test]
fn key_value_column_rewrites_upgrade_legacy_documents_without_type_loss() {
    let store = store();
    store
        .put(
            &document_key("articles", 7).unwrap(),
            br#"{"legacy_bytes":[1,2],"legacy_empty":[],"legacy_list":[1,300]}"#,
        )
        .unwrap();
    let mut docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
    docs.put(
        8,
        BTreeMap::from([
            (
                "new_list".into(),
                Value::List(vec![Value::Int(1), Value::Int(2)]),
            ),
            ("new_bytes".into(), Value::Bytes(vec![1, 2])),
            ("drop_me".into(), Value::Str("removed".into())),
        ]),
    )
    .unwrap();

    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog
        .rename_column_data("articles", "legacy_bytes", "renamed_bytes")
        .unwrap();
    catalog.drop_column_data("articles", "drop_me").unwrap();
    drop(docs);

    let docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
    let legacy = docs.get(7).unwrap().unwrap();
    assert_eq!(legacy["renamed_bytes"], Value::Bytes(vec![1, 2]));
    assert_eq!(legacy["legacy_empty"], Value::Bytes(Vec::new()));
    assert_eq!(
        legacy["legacy_list"],
        Value::List(vec![Value::Int(1), Value::Int(300)])
    );
    let current = docs.get(8).unwrap().unwrap();
    assert_eq!(
        current["new_list"],
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(current["new_bytes"], Value::Bytes(vec![1, 2]));
    assert!(!current.contains_key("drop_me"));
    for doc_id in [7, 8] {
        assert!(store
            .get(&document_key("articles", doc_id).unwrap())
            .unwrap()
            .unwrap()
            .starts_with(DOCUMENT_VALUE_V1_PREFIX));
    }
}

#[test]
fn key_value_inverted_index_replaces_and_removes_documents() {
    let mut index = KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
    index
        .add_document(1, BTreeMap::from([("title".into(), "rust rust".into())]))
        .unwrap();
    index
        .add_document(2, BTreeMap::from([("title".into(), "rust search".into())]))
        .unwrap();
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 2);
    assert_eq!(index.get_term_freq(1, "title", "rust").unwrap(), 2);
    assert_eq!(index.total_field_length("title").unwrap(), 4);

    index
        .add_document(1, BTreeMap::from([("title".into(), "sqlite".into())]))
        .unwrap();
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 1);
    assert_eq!(index.total_field_length("title").unwrap(), 3);

    index.remove_document(2).unwrap();
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(index.total_field_length("title").unwrap(), 1);
}

fn seed_paged_legacy_postings(store: &dyn KeyValueStore) -> Vec<u64> {
    let mut document_ids = (1_u64..=1_030).collect::<Vec<_>>();
    document_ids.push(65_536);
    for doc_id in &document_ids {
        store
            .put(
                &doc_length_key("articles", *doc_id, "title").unwrap(),
                &u64_value(4),
            )
            .unwrap();
        store
            .put(
                &posting_key("articles", "title", "rust", *doc_id).unwrap(),
                &positions_to_blob(&[0]).unwrap(),
            )
            .unwrap();
        store
            .put(
                &reverse_posting_key("articles", *doc_id, "title", "rust").unwrap(),
                &[],
            )
            .unwrap();
    }
    store
        .put(
            &posting_key("articles", "title", "search", 1).unwrap(),
            &positions_to_blob(&[1, 3]).unwrap(),
        )
        .unwrap();
    store
        .put(
            &reverse_posting_key("articles", 1, "title", "search").unwrap(),
            &[],
        )
        .unwrap();
    for term in ["z", "aa"] {
        store
            .put(
                &posting_key("articles", "title", term, 1).unwrap(),
                &positions_to_blob(&[2]).unwrap(),
            )
            .unwrap();
        store
            .put(
                &reverse_posting_key("articles", 1, "title", term).unwrap(),
                &[],
            )
            .unwrap();
    }
    store
        .put(
            &field_stats_key("articles", "title").unwrap(),
            &u64_value(document_ids.len() as u64 * 4),
        )
        .unwrap();
    document_ids
}

#[test]
fn key_value_legacy_postings_migrate_atomically_across_scan_pages() {
    let store = store();
    let document_ids = seed_paged_legacy_postings(store.as_ref());

    KeyValueInvertedIndex::migrate_legacy_storage(store.as_ref()).unwrap();

    assert!(store
        .scan_prefix(&posting_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&reverse_posting_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .scan_prefix(&posting_cluster_score_key_prefix("articles").unwrap())
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        store
            .scan_prefix(&posting_cluster_positions_key_prefix("articles").unwrap())
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        store
            .scan_prefix(&posting_document_key_prefix("articles").unwrap())
            .unwrap()
            .len(),
        document_ids.len()
    );
    let terms = decode_terms(
        &store
            .get(&posting_document_key("articles", 1, "title").unwrap())
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        terms,
        vec![
            "aa".to_string(),
            "rust".to_string(),
            "search".to_string(),
            "z".to_string()
        ]
    );

    let index =
        KeyValueInvertedIndex::new(Arc::clone(&store), "articles", standard_analyzer("english"));
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 1_031);
    assert_eq!(index.get_term_freq(1, "title", "search").unwrap(), 2);
    assert_eq!(
        index
            .get_posting_list("title", "rust")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        document_ids
    );

    let version_before = store.change_version().unwrap();
    KeyValueInvertedIndex::migrate_legacy_storage(store.as_ref()).unwrap();
    assert_eq!(store.change_version().unwrap(), version_before);
    assert_eq!(
        decode_string(
            store
                .get(&single_str_key(TAG_METADATA, "inverted_index_format").unwrap())
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        "clustered-v1"
    );
}

#[test]
fn key_value_legacy_posting_migration_rolls_back_on_missing_reverse_key() {
    let store = store();
    let legacy_key = posting_key("articles", "title", "rust", 7).unwrap();
    let legacy_value = positions_to_blob(&[0, 2]).unwrap();
    store
        .put(
            &doc_length_key("articles", 7, "title").unwrap(),
            &u64_value(3),
        )
        .unwrap();
    store.put(&legacy_key, &legacy_value).unwrap();

    let error = KeyValueInvertedIndex::migrate_legacy_storage(store.as_ref()).unwrap_err();
    assert!(error.to_string().contains("missing reverse posting"));
    assert_eq!(store.get(&legacy_key).unwrap(), Some(legacy_value));
    assert!(store
        .scan_prefix(&posting_cluster_score_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_cluster_positions_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .get(&single_str_key(TAG_METADATA, "inverted_index_format").unwrap())
        .unwrap()
        .is_none());
    assert!(!store.in_transaction());
}

#[test]
fn key_value_posting_migration_rejects_an_unknown_format_marker() {
    let store = store();
    let marker = single_str_key(TAG_METADATA, "inverted_index_format").unwrap();
    store.put(&marker, b"clustered-v2").unwrap();

    let error = KeyValueInvertedIndex::migrate_legacy_storage(store.as_ref()).unwrap_err();
    assert!(error
        .to_string()
        .contains("unsupported KeyValue inverted-index format `clustered-v2`"));
    assert_eq!(store.get(&marker).unwrap(), Some(b"clustered-v2".to_vec()));
    assert!(!store.in_transaction());
}

#[test]
fn clustered_postings_follow_key_value_column_lifecycle() {
    let store = store();
    let mut index =
        KeyValueInvertedIndex::new(Arc::clone(&store), "articles", standard_analyzer("english"));
    index
        .add_document(
            1,
            BTreeMap::from([
                ("title".into(), "rust search".into()),
                ("body".into(), "sqlite storage".into()),
            ]),
        )
        .unwrap();
    index
        .add_document(2, BTreeMap::from([("title".into(), "rust".into())]))
        .unwrap();

    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog
        .rename_column_data("articles", "title", "headline")
        .unwrap();
    catalog.drop_column_data("articles", "body").unwrap();

    let mut renamed =
        KeyValueInvertedIndex::new(Arc::clone(&store), "articles", standard_analyzer("english"));
    assert_eq!(renamed.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(renamed.doc_freq("headline", "rust").unwrap(), 2);
    assert_eq!(renamed.doc_freq("body", "sqlite").unwrap(), 0);
    assert_eq!(renamed.get_doc_length(1, "headline").unwrap(), 2);
    assert_eq!(renamed.get_doc_length(1, "body").unwrap(), 0);
    renamed.remove_document(1).unwrap();
    assert_eq!(renamed.doc_freq("headline", "rust").unwrap(), 1);
    assert_eq!(renamed.doc_freq("headline", "search").unwrap(), 0);
    assert!(store
        .scan_prefix(&posting_cluster_score_field_prefix("articles", "title").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_cluster_positions_field_prefix("articles", "body").unwrap())
        .unwrap()
        .is_empty());

    catalog.purge_table_data("articles").unwrap();
    assert!(store
        .scan_prefix(&posting_cluster_score_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_cluster_positions_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_document_key_prefix("articles").unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn clustered_postings_follow_key_value_table_rename_and_drop() {
    let store = store();
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "articles"),
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec!["title".into()],
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    let mut index = KeyValueInvertedIndex::new(
        Arc::clone(&store),
        "public.articles",
        standard_analyzer("english"),
    );
    index
        .add_document(1, BTreeMap::from([("title".into(), "rust search".into())]))
        .unwrap();

    catalog
        .rename_table_data("public.articles", "public.docs")
        .unwrap();
    let renamed = KeyValueInvertedIndex::new(
        Arc::clone(&store),
        "public.docs",
        standard_analyzer("english"),
    );
    assert_eq!(renamed.doc_freq("title", "rust").unwrap(), 1);
    assert!(store
        .scan_prefix(&posting_cluster_score_key_prefix("public.articles").unwrap())
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .scan_prefix(&posting_document_key_prefix("public.docs").unwrap())
            .unwrap()
            .len(),
        1
    );

    catalog.drop_table_and_data("public.docs").unwrap();
    assert!(store
        .scan_prefix(&posting_cluster_score_key_prefix("public.docs").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_cluster_positions_key_prefix("public.docs").unwrap())
        .unwrap()
        .is_empty());
    assert!(store
        .scan_prefix(&posting_document_key_prefix("public.docs").unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn key_value_add_counter_overflow_is_atomic() {
    let store = store();
    let mut index =
        KeyValueInvertedIndex::new(Arc::clone(&store), "articles", standard_analyzer("english"));
    index
        .add_document(1, BTreeMap::from([("title".into(), "rust".into())]))
        .unwrap();
    store
        .put(
            &field_stats_key("articles", "title").unwrap(),
            &u64_value(u64::MAX),
        )
        .unwrap();

    let error = index
        .add_document(2, BTreeMap::from([("title".into(), "sqlite".into())]))
        .unwrap_err();
    assert!(error.to_string().contains("total field length overflow"));
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 0);
}

#[test]
fn key_value_rebuild_analysis_failure_preserves_old_index() {
    let store = store();
    let mut index = KeyValueInvertedIndex::new(store, "articles", standard_analyzer("english"));
    index
        .add_document(1, BTreeMap::from([("title".into(), "rust".into())]))
        .unwrap();
    let invalid = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 0,
            max_gram: 1,
        },
        Vec::new(),
        Vec::new(),
    );
    index
        .set_field_analyzer("body", invalid, AnalyzerPhase::Index)
        .unwrap();

    let error = index
        .try_rebuild_documents(vec![
            (2, BTreeMap::from([("title".into(), "sqlite".into())])),
            (3, BTreeMap::from([("body".into(), "failure".into())])),
        ])
        .unwrap_err();
    assert!(error.to_string().contains("gram"));
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 0);
}

#[test]
fn key_value_batch_analysis_failure_preserves_old_index() {
    let store = store();
    let mut index = KeyValueInvertedIndex::new(store, "articles", standard_analyzer("english"));
    index
        .add_document(1, BTreeMap::from([("title".into(), "rust".into())]))
        .unwrap();
    let invalid = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 0,
            max_gram: 1,
        },
        Vec::new(),
        Vec::new(),
    );
    index
        .set_field_analyzer("body", invalid, AnalyzerPhase::Index)
        .unwrap();

    let error = index
        .try_add_documents(vec![
            (2, BTreeMap::from([("title".into(), "sqlite".into())])),
            (3, BTreeMap::from([("body".into(), "failure".into())])),
        ])
        .unwrap_err();
    assert!(error.to_string().contains("gram"));
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(index.doc_freq("title", "sqlite").unwrap(), 0);
}

#[test]
fn key_value_batch_coalesces_replacements_removals_and_clusters() {
    let mut index = KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
    index
        .add_document(
            1,
            BTreeMap::from([
                ("title".into(), "old token".into()),
                ("body".into(), "obsolete".into()),
            ]),
        )
        .unwrap();
    index
        .add_document(2, BTreeMap::from([("title".into(), "keep rust".into())]))
        .unwrap();
    let distant_doc = crate::POSTING_CLUSTER_DOCS + 5;

    index
        .try_add_documents(vec![
            (1, BTreeMap::from([("title".into(), "new".into())])),
            (2, BTreeMap::new()),
            (3, BTreeMap::from([("title".into(), "discarded".into())])),
            (3, BTreeMap::from([("title".into(), "new rust".into())])),
            (
                distant_doc,
                BTreeMap::from([("title".into(), "rust".into())]),
            ),
        ])
        .unwrap();

    assert_eq!(
        index
            .get_posting_list("title", "new")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        index
            .get_posting_list("title", "rust")
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![3, distant_doc]
    );
    for removed in ["old", "token", "keep", "discard"] {
        assert_eq!(index.doc_freq("title", removed).unwrap(), 0);
    }
    assert_eq!(index.doc_length_count(Some("title")).unwrap(), 3);
    assert_eq!(index.total_field_length("title").unwrap(), 4);
    assert_eq!(index.doc_length_count(Some("body")).unwrap(), 0);
    assert_eq!(index.total_field_length("body").unwrap(), 0);
}

#[test]
fn key_value_field_stats_and_vocabulary_are_field_scoped() {
    let mut index = KeyValueInvertedIndex::new(store(), "articles", standard_analyzer("english"));
    index
        .add_document(
            1,
            BTreeMap::from([
                ("title".into(), "rust search".into()),
                ("body".into(), "long body text here".into()),
            ]),
        )
        .unwrap();
    index
        .add_document(2, BTreeMap::from([("title".into(), "sqlite".into())]))
        .unwrap();

    let title_stats = index.field_stats("title").unwrap();
    assert_eq!(title_stats.total_docs, 2);
    assert_eq!(title_stats.avg_doc_length, 1.5);
    assert_eq!(
        index.vocabulary_terms("title").unwrap(),
        vec![
            "rust".to_string(),
            "search".to_string(),
            "sqlite".to_string()
        ]
    );
    assert_eq!(index.field_stats("body").unwrap().total_docs, 1);
}

#[test]
fn key_value_vector_reader_rejects_corrupt_ordinal() {
    let store = store();
    let mut key = vector_doc_prefix("articles", "embedding", 1).unwrap();
    push_u64(&mut key, u64::MAX);
    store
        .put(&key, &vector_to_blob(&[1.0, 0.0]).unwrap())
        .unwrap();
    let index = KeyValueVectorIndex::new(store, "articles", "embedding", 2);

    let error = index.search_knn(&[1.0, 0.0], 1).unwrap_err();
    assert!(error.to_string().contains("persisted vector ordinal"));
}

#[test]
fn key_value_catalog_preserves_core_registries() {
    let catalog = KeyValueCatalog::new(store());
    catalog.set_metadata("schema_version", "10").unwrap();
    assert_eq!(
        catalog.get_metadata("schema_version").unwrap().as_deref(),
        Some("10")
    );
    catalog.save_schema("public").unwrap();
    catalog.save_schema("empty_app").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "docs"),
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec!["title".into()],
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    catalog.save_model("reranker", "{\"model\":1}").unwrap();
    catalog
        .save_scoring_params("bm25", "{\"alpha\":0.5}")
        .unwrap();
    catalog.save_named_graph("g").unwrap();
    catalog.save_vertex(1, "Person", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();

    assert_eq!(
        catalog.load_tables().unwrap()[0].relation.qualified_name(),
        "public.docs"
    );
    assert_eq!(
        catalog.load_model("reranker").unwrap().as_deref(),
        Some("{\"model\":1}")
    );
    assert_eq!(catalog.load_named_graphs().unwrap(), vec!["g"]);
    assert_eq!(
        catalog.load_schemas().unwrap(),
        vec!["empty_app".to_string(), "public".to_string()]
    );
    assert_eq!(catalog.load_vertices().unwrap()[0].0, 1);
    assert_eq!(
        catalog.load_graph_memberships().unwrap(),
        vec![("vertex".into(), 1, "g".into())]
    );
}
