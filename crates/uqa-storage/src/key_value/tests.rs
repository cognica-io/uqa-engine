use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::Value;

use crate::document_store::DocumentStore;
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::vector_index::VectorIndex;

use super::codec::*;
use super::{
    KeyValueCatalog, KeyValueDocumentStore, KeyValueInvertedIndex, KeyValueStore,
    KeyValueVectorIndex, MemoryKeyValueStore, DOCUMENT_VALUE_V1_PREFIX,
};
use crate::StorageBackendError;

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

#[test]
fn key_value_column_stats_replace_is_a_complete_batch() {
    let catalog = KeyValueCatalog::new(store());
    catalog
        .save_column_stats(crate::catalog::ColumnStatsInput::basic(
            "docs", "old", 1, 0, None, None, 1,
        ))
        .unwrap();
    let replacement = [
        crate::catalog::ColumnStatsInput::basic("docs", "a", 2, 0, None, None, 3),
        crate::catalog::ColumnStatsInput::basic("docs", "b", 3, 1, None, None, 3),
    ];

    catalog.replace_column_stats("docs", &replacement).unwrap();
    assert_eq!(
        catalog
            .load_column_stats("docs")
            .unwrap()
            .into_iter()
            .map(|row| row.column_name)
            .collect::<Vec<_>>(),
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn key_value_drop_cleans_only_its_legacy_public_alias() {
    let catalog = KeyValueCatalog::new(store());
    catalog.save_schema("public").unwrap();
    catalog.save_schema("app").unwrap();
    for (schema, name) in [("public", "docs"), ("app", "docs")] {
        catalog
            .save_table(&TableSchema {
                relation: crate::catalog::RelationIdentity::new(schema, name),
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
    }
    for table_name in ["public.docs", "docs", "app.docs"] {
        catalog
            .save_column_stats(crate::catalog::ColumnStatsInput::basic(
                table_name, "id", 1, 0, None, None, 1,
            ))
            .unwrap();
    }

    catalog.drop_table_and_data("public.docs").unwrap();

    assert!(catalog.load_column_stats("public.docs").unwrap().is_empty());
    assert!(catalog.load_column_stats("docs").unwrap().is_empty());
    assert_eq!(catalog.load_column_stats("app.docs").unwrap().len(), 1);
    assert_eq!(
        catalog.load_tables().unwrap()[0].relation.qualified_name(),
        "app.docs"
    );
}

#[test]
fn key_value_column_lifecycle_rejects_corrupt_catalog_index_columns() {
    let catalog = KeyValueCatalog::new(store());
    catalog
        .save_catalog_index("broken", "btree", "docs", "not-json", "{}")
        .unwrap();

    assert!(matches!(
        catalog.drop_column_data("docs", "title"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
    assert!(matches!(
        catalog.rename_column_data("docs", "title", "headline"),
        Err(StorageBackendError::Serde(_))
    ));
    assert_eq!(
        catalog.load_catalog_indexes().unwrap()[0].columns_json,
        "not-json"
    );
}
