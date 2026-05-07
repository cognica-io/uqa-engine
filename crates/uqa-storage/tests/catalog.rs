//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_catalog.py` covering the SQLite-backed
//! `Catalog` plus the per-module persistence stores it orchestrates.
//!
//! The Python reference exposes one fat `Catalog` class with methods
//! like `save_document`, `save_postings`, `save_vector`. The Rust port
//! splits those concerns across `Catalog`, `SQLiteDocumentStore`,
//! `SQLiteInvertedIndex`, and `SQLiteVectorIndex`. Tests below exercise
//! the same observable behaviour through the Rust API surface.

use std::collections::BTreeMap;

use uqa_analysis::standard_analyzer;
use uqa_core::{DocId, Value};
use uqa_storage::document_store::DocumentStore;
use uqa_storage::inverted_index::InvertedIndex;
use uqa_storage::sqlite::{
    Catalog, ColumnStatsRow, ManagedConnection, SQLiteDocumentStore, SQLiteInvertedIndex,
    SQLiteVectorIndex, TableSchema, VectorFieldSchema,
};
use uqa_storage::vector_index::VectorIndex;

// =====================================================================
// Helpers
// =====================================================================

fn open_catalog(path: &std::path::Path) -> (ManagedConnection, Catalog) {
    let conn = ManagedConnection::open(path).unwrap();
    let cat = Catalog::open(conn.clone()).unwrap();
    (conn, cat)
}

fn open_in_memory_catalog() -> (ManagedConnection, Catalog) {
    let conn = ManagedConnection::open_in_memory().unwrap();
    let cat = Catalog::open(conn.clone()).unwrap();
    (conn, cat)
}

fn document_store(conn: ManagedConnection, table: &str) -> SQLiteDocumentStore {
    SQLiteDocumentStore::new(conn, table.to_string())
}

fn inverted_index(conn: ManagedConnection, table: &str) -> SQLiteInvertedIndex {
    SQLiteInvertedIndex::new(conn, table.to_string(), standard_analyzer("english"))
}

fn vector_index(conn: ManagedConnection, table: &str, field: &str, dim: u32) -> SQLiteVectorIndex {
    SQLiteVectorIndex::new(conn, table.to_string(), field.to_string(), dim)
}

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn schema(name: &str, fts: &[&str]) -> TableSchema {
    TableSchema {
        name: name.to_string(),
        analyzer_json: r#"{"name":"standard","language":"english"}"#.to_string(),
        fts_fields: fts.iter().map(|s| (*s).to_string()).collect(),
        vector_fields: Vec::new(),
        columns_json: String::new(),
    }
}

fn schema_with_columns(name: &str, columns_json: &str) -> TableSchema {
    TableSchema {
        name: name.to_string(),
        analyzer_json: r#"{"name":"standard","language":"english"}"#.to_string(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: columns_json.to_string(),
    }
}

fn doc<const N: usize>(pairs: [(&str, Value); N]) -> uqa_storage::document_store::Document {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

// =====================================================================
// TestCatalogMetadata
// =====================================================================

#[test]
fn metadata_set_and_get() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.set_metadata("key1", "value1").unwrap();
    assert_eq!(cat.get_metadata("key1").unwrap().as_deref(), Some("value1"));
}

#[test]
fn metadata_get_missing_returns_none() {
    let (_conn, cat) = open_in_memory_catalog();
    assert!(cat.get_metadata("nonexistent").unwrap().is_none());
}

#[test]
fn metadata_overwrite() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.set_metadata("k", "v1").unwrap();
    cat.set_metadata("k", "v2").unwrap();
    assert_eq!(cat.get_metadata("k").unwrap().as_deref(), Some("v2"));
}

// =====================================================================
// TestCatalogTableSchemas
// =====================================================================

#[test]
fn table_schema_save_and_load_round_trip() {
    let (_conn, cat) = open_in_memory_catalog();
    let columns_json = r#"[
        {"name":"id","type":"Integer","primary_key":true,"not_null":true,"auto_increment":true},
        {"name":"title","type":"Text","primary_key":false,"not_null":true,"auto_increment":false}
    ]"#;
    cat.save_table(&schema_with_columns("papers", columns_json))
        .unwrap();
    let loaded = cat.load_tables().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "papers");
    assert_eq!(loaded[0].columns_json, columns_json);
}

#[test]
fn drop_table_removes_schema_and_documents() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &[])).unwrap();
    let mut store = document_store(conn.clone(), "t1");
    store.put(1, doc([("a", Value::Int(10))]));
    store.put(2, doc([("a", Value::Int(20))]));
    cat.drop_table("t1").unwrap();
    cat.purge_table_data("t1").unwrap();
    assert!(cat.load_tables().unwrap().is_empty());
    assert!(store.doc_ids().is_empty());
}

#[test]
fn drop_table_cascades_postings_and_stats() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["x"])).unwrap();
    let mut idx = inverted_index(conn.clone(), "t1");
    idx.add_document(1, fields(&[("x", "hello")]));
    cat.save_column_stats("t1", "x", 5, 0, Some("a"), Some("z"), 10)
        .unwrap();
    cat.drop_table("t1").unwrap();
    cat.purge_table_data("t1").unwrap();
    assert_eq!(idx.get_posting_list("x", "hello").len(), 0);
    assert_eq!(idx.get_doc_length(1, "x"), 0);
    assert!(cat.load_column_stats("t1").unwrap().is_empty());
}

#[test]
fn multiple_tables_round_trip() {
    let (_conn, cat) = open_in_memory_catalog();
    for name in ["t1", "t2", "t3"] {
        cat.save_table(&schema(name, &[])).unwrap();
    }
    let names: std::collections::BTreeSet<_> = cat
        .load_tables()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        ["t1".to_string(), "t2".to_string(), "t3".to_string()]
            .into_iter()
            .collect()
    );
}

// =====================================================================
// TestCatalogDocuments (via SQLiteDocumentStore)
// =====================================================================

#[test]
fn document_save_and_load() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut store = document_store(conn, "t1");
    store.put(
        1,
        doc([
            ("name", Value::Str("alice".into())),
            ("age", Value::Int(30)),
        ]),
    );
    store.put(
        2,
        doc([("name", Value::Str("bob".into())), ("age", Value::Int(25))]),
    );
    let d1 = store.get(1).unwrap();
    let d2 = store.get(2).unwrap();
    assert_eq!(d1.get("name"), Some(&Value::Str("alice".into())));
    assert_eq!(d2.get("name"), Some(&Value::Str("bob".into())));
}

#[test]
fn documents_tables_isolated() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut s1 = document_store(conn.clone(), "t1");
    let mut s2 = document_store(conn, "t2");
    s1.put(1, doc([("x", Value::Int(1))]));
    s2.put(1, doc([("x", Value::Int(2))]));
    assert_eq!(s1.get(1).unwrap().get("x"), Some(&Value::Int(1)));
    assert_eq!(s2.get(1).unwrap().get("x"), Some(&Value::Int(2)));
}

#[test]
fn document_delete_removes_row() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut store = document_store(conn, "t1");
    store.put(1, doc([("x", Value::Int(1))]));
    store.put(2, doc([("x", Value::Int(2))]));
    store.delete(1);
    let mut ids = store.doc_ids();
    ids.sort();
    assert_eq!(ids, vec![2u64 as DocId]);
}

#[test]
fn document_delete_cascades_postings() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["x"])).unwrap();
    let mut store = document_store(conn.clone(), "t1");
    let mut idx = inverted_index(conn, "t1");
    store.put(1, doc([("x", Value::Str("hello".into()))]));
    idx.add_document(1, fields(&[("x", "hello")]));
    store.delete(1);
    idx.remove_document(1);
    assert!(store.get(1).is_none());
    assert_eq!(idx.get_posting_list("x", "hello").len(), 0);
    assert_eq!(idx.get_doc_length(1, "x"), 0);
}

#[test]
fn document_upsert_overwrites() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut store = document_store(conn, "t1");
    store.put(1, doc([("v", Value::Str("old".into()))]));
    store.put(1, doc([("v", Value::Str("new".into()))]));
    assert_eq!(store.len(), 1);
    assert_eq!(
        store.get(1).unwrap().get("v"),
        Some(&Value::Str("new".into()))
    );
}

// =====================================================================
// TestCatalogPostings (via SQLiteInvertedIndex)
// =====================================================================

#[test]
fn postings_save_and_load_round_trip() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["title", "body"])).unwrap();
    let mut idx = inverted_index(conn, "t1");
    idx.add_document(
        1,
        fields(&[("title", "hello world"), ("body", "hello hello world")]),
    );
    let pl_title = idx.get_posting_list("title", "hello");
    let pl_body = idx.get_posting_list("body", "hello");
    assert_eq!(pl_title.len(), 1);
    assert_eq!(pl_body.entries()[0].payload.positions.len(), 2);
}

#[test]
fn postings_doc_lengths_round_trip() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["title", "body"])).unwrap();
    let mut idx = inverted_index(conn, "t1");
    // The standard English analyzer drops stop words ("a", "the", ...)
    // so the test corpus uses content tokens only.
    idx.add_document(
        1,
        fields(&[
            ("title", "alpha bravo charlie"),
            ("body", "alpha bravo charlie delta echo"),
        ]),
    );
    idx.add_document(2, fields(&[("title", "alpha bravo")]));
    assert_eq!(idx.get_doc_length(1, "title"), 3);
    assert_eq!(idx.get_doc_length(1, "body"), 5);
    assert_eq!(idx.get_doc_length(2, "title"), 2);
}

#[test]
fn postings_delete_removes_row_only() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["x"])).unwrap();
    let mut idx = inverted_index(conn, "t1");
    idx.add_document(1, fields(&[("x", "alpha")]));
    idx.add_document(2, fields(&[("x", "bravo")]));
    idx.remove_document(1);
    assert_eq!(idx.get_posting_list("x", "alpha").len(), 0);
    assert_eq!(idx.get_posting_list("x", "bravo").len(), 1);
    assert_eq!(idx.get_doc_length(1, "x"), 0);
    assert_eq!(idx.get_doc_length(2, "x"), 1);
}

#[test]
fn postings_tables_isolated() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&schema("t1", &["x"])).unwrap();
    cat.save_table(&schema("t2", &["x"])).unwrap();
    let mut i1 = inverted_index(conn.clone(), "t1");
    let mut i2 = inverted_index(conn, "t2");
    i1.add_document(1, fields(&[("x", "alpha")]));
    i2.add_document(1, fields(&[("x", "bravo")]));
    assert_eq!(i1.get_posting_list("x", "alpha").len(), 1);
    assert_eq!(i1.get_posting_list("x", "bravo").len(), 0);
    assert_eq!(i2.get_posting_list("x", "alpha").len(), 0);
    assert_eq!(i2.get_posting_list("x", "bravo").len(), 1);
}

// =====================================================================
// TestCatalogGraph
// =====================================================================

#[test]
fn graph_vertices_round_trip() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_vertex(1, "", r#"{"name":"A"}"#).unwrap();
    cat.save_vertex(2, "", r#"{"name":"B"}"#).unwrap();
    let verts = cat.load_vertices().unwrap();
    assert_eq!(verts.len(), 2);
    let by_id: std::collections::BTreeMap<u64, String> = verts
        .into_iter()
        .map(|(id, _, props)| (id, props))
        .collect();
    assert_eq!(by_id.get(&1).map(|s| s.as_str()), Some(r#"{"name":"A"}"#));
    assert_eq!(by_id.get(&2).map(|s| s.as_str()), Some(r#"{"name":"B"}"#));
}

#[test]
fn graph_edges_round_trip() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_edge(10, 1, 2, "knows", r#"{"weight":0.5}"#)
        .unwrap();
    let edges = cat.load_edges().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].edge_id, 10);
    assert_eq!(edges[0].source_id, 1);
    assert_eq!(edges[0].target_id, 2);
    assert_eq!(edges[0].label, "knows");
    assert_eq!(edges[0].properties_json, r#"{"weight":0.5}"#);
}

// =====================================================================
// TestCatalogVectors (via SQLiteVectorIndex)
// =====================================================================

#[test]
fn vector_round_trip() {
    let (conn, cat) = open_in_memory_catalog();
    cat.save_table(&TableSchema {
        name: "t".into(),
        analyzer_json: r#"{"name":"standard","language":"english"}"#.into(),
        fts_fields: Vec::new(),
        vector_fields: vec![VectorFieldSchema {
            field: "emb".into(),
            dimensions: 3,
        }],
        columns_json: String::new(),
    })
    .unwrap();
    let mut idx = vector_index(conn, "t", "emb", 3);
    idx.add(1, vec![1.0, 2.0, 3.0]);
    let pl = idx.search_knn(&[1.0, 2.0, 3.0], 1);
    assert_eq!(pl.len(), 1);
    assert_eq!(pl.entries()[0].doc_id, 1);
}

#[test]
fn vector_multiple_documents_round_trip() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut idx = vector_index(conn, "t", "emb", 4);
    for i in 0..5u64 {
        idx.add(
            i as DocId,
            vec![i as f32, (i * 2) as f32, (i * 3) as f32, (i * 4) as f32],
        );
    }
    assert_eq!(idx.count(), 5);
}

#[test]
fn vector_delete_removes_only_target() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut idx = vector_index(conn, "t", "emb", 2);
    idx.add(1, vec![1.0, 2.0]);
    idx.add(2, vec![3.0, 4.0]);
    idx.delete(1);
    assert_eq!(idx.count(), 1);
    let pl = idx.search_knn(&[3.0, 4.0], 1);
    assert_eq!(pl.entries()[0].doc_id, 2);
}

// =====================================================================
// TestCatalogColumnStats
// =====================================================================

#[test]
fn column_stats_save_and_load() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_column_stats("t1", "age", 10, 2, Some("18"), Some("65"), 100)
        .unwrap();
    cat.save_column_stats("t1", "name", 50, 0, Some("alice"), Some("zoe"), 100)
        .unwrap();
    let stats = cat.load_column_stats("t1").unwrap();
    assert_eq!(stats.len(), 2);
    let by_col: std::collections::BTreeMap<String, ColumnStatsRow> = stats
        .into_iter()
        .map(|s| (s.column_name.clone(), s))
        .collect();
    assert_eq!(by_col["age"].distinct_count, 10);
    assert_eq!(by_col["age"].null_count, 2);
    assert_eq!(by_col["age"].min_value.as_deref(), Some("18"));
    assert_eq!(by_col["age"].max_value.as_deref(), Some("65"));
    assert_eq!(by_col["age"].row_count, 100);
    assert_eq!(by_col["age"].histogram_json, "[]");
    assert_eq!(by_col["age"].mcv_values_json, "[]");
    assert_eq!(by_col["age"].mcv_frequencies_json, "[]");
    assert_eq!(by_col["name"].distinct_count, 50);
}

#[test]
fn column_stats_full_round_trip_preserves_histogram_and_mcv() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_column_stats_full(
        "t1",
        "cat",
        2,
        0,
        Some(r#""A""#),
        Some(r#""B""#),
        100,
        r#"["A","B"]"#,
        r#"["A"]"#,
        r#"[0.6]"#,
    )
    .unwrap();
    let stats = cat.load_column_stats("t1").unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].min_value.as_deref(), Some(r#""A""#));
    assert_eq!(stats[0].max_value.as_deref(), Some(r#""B""#));
    assert_eq!(stats[0].histogram_json, r#"["A","B"]"#);
    assert_eq!(stats[0].mcv_values_json, r#"["A"]"#);
    assert_eq!(stats[0].mcv_frequencies_json, r#"[0.6]"#);
}

#[test]
fn column_stats_overwrite() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_column_stats("t1", "x", 5, 0, Some("1"), Some("10"), 20)
        .unwrap();
    cat.save_column_stats("t1", "x", 8, 1, Some("2"), Some("15"), 30)
        .unwrap();
    let stats = cat.load_column_stats("t1").unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].distinct_count, 8);
    assert_eq!(stats[0].null_count, 1);
    assert_eq!(stats[0].row_count, 30);
}

#[test]
fn column_stats_delete() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_column_stats("t1", "x", 5, 0, Some("1"), Some("10"), 20)
        .unwrap();
    cat.delete_column_stats("t1").unwrap();
    assert!(cat.load_column_stats("t1").unwrap().is_empty());
}

#[test]
fn column_stats_tables_isolated() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_column_stats("t1", "x", 5, 0, Some("1"), Some("10"), 20)
        .unwrap();
    cat.save_column_stats("t2", "x", 8, 0, Some("1"), Some("20"), 40)
        .unwrap();
    let s1 = cat.load_column_stats("t1").unwrap();
    let s2 = cat.load_column_stats("t2").unwrap();
    assert_eq!(s1.len(), 1);
    assert_eq!(s2.len(), 1);
    assert_eq!(s1[0].distinct_count, 5);
    assert_eq!(s2[0].distinct_count, 8);
}

// =====================================================================
// TestCatalogScoringParams
// =====================================================================

#[test]
fn scoring_params_save_and_load() {
    let (_conn, cat) = open_in_memory_catalog();
    let json = r#"{"alpha":1.5,"beta":0.3,"base_rate":0.01}"#;
    cat.save_scoring_params("bm25_body", json).unwrap();
    assert_eq!(
        cat.load_scoring_params("bm25_body").unwrap().as_deref(),
        Some(json)
    );
}

#[test]
fn scoring_params_load_missing_returns_none() {
    let (_conn, cat) = open_in_memory_catalog();
    assert!(cat.load_scoring_params("nonexistent").unwrap().is_none());
}

#[test]
fn scoring_params_overwrite() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_scoring_params("sig1", r#"{"alpha":1.0}"#).unwrap();
    cat.save_scoring_params("sig1", r#"{"alpha":2.0,"beta":0.5}"#)
        .unwrap();
    assert_eq!(
        cat.load_scoring_params("sig1").unwrap().as_deref(),
        Some(r#"{"alpha":2.0,"beta":0.5}"#)
    );
}

#[test]
fn scoring_params_load_all() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_scoring_params("s1", r#"{"alpha":1.0}"#).unwrap();
    cat.save_scoring_params("s2", r#"{"alpha":2.0}"#).unwrap();
    let all = cat.load_all_scoring_params().unwrap();
    assert_eq!(all.len(), 2);
    let names: std::collections::BTreeSet<_> = all.into_iter().map(|(n, _)| n).collect();
    assert!(names.contains("s1"));
    assert!(names.contains("s2"));
}

#[test]
fn scoring_params_delete() {
    let (_conn, cat) = open_in_memory_catalog();
    cat.save_scoring_params("s1", r#"{"alpha":1.0}"#).unwrap();
    cat.drop_scoring_params("s1").unwrap();
    assert!(cat.load_scoring_params("s1").unwrap().is_none());
}

// =====================================================================
// TestCatalogTransactions (via ManagedConnection)
// =====================================================================

#[test]
fn transaction_batch_commit() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut store = document_store(conn.clone(), "t1");
    conn.begin_transaction().unwrap();
    for i in 1..=3u64 {
        store.put(i as DocId, doc([("x", Value::Int(i as i64))]));
    }
    conn.commit_transaction().unwrap();
    assert_eq!(store.len(), 3);
}

#[test]
fn transaction_rollback_drops_uncommitted_writes() {
    let (conn, _cat) = open_in_memory_catalog();
    let mut store = document_store(conn.clone(), "t1");
    store.put(1, doc([("x", Value::Int(1))]));
    conn.begin_transaction().unwrap();
    store.put(2, doc([("x", Value::Int(2))]));
    conn.rollback_transaction().unwrap();
    let mut ids = store.doc_ids();
    ids.sort();
    assert_eq!(ids, vec![1u64 as DocId]);
}

#[test]
fn auto_commit_outside_transaction_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    {
        let (conn, _cat) = open_catalog(&path);
        let mut store = document_store(conn, "t1");
        store.put(1, doc([("x", Value::Int(1))]));
    }
    let (conn2, _cat2) = open_catalog(&path);
    let store2 = document_store(conn2, "t1");
    assert_eq!(store2.len(), 1);
}

// =====================================================================
// TestCatalogPersistence -- close + reopen restores every store
// =====================================================================

#[test]
fn close_and_reopen_restores_every_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");

    let lengths_before;
    let posting_count_before;
    {
        let (conn, cat) = open_catalog(&path);
        cat.save_table(&schema("t", &["x"])).unwrap();
        let mut store = document_store(conn.clone(), "t");
        let mut idx = inverted_index(conn.clone(), "t");
        let mut vec_idx = vector_index(conn.clone(), "t", "emb", 2);

        store.put(1, doc([("x", Value::Int(42))]));
        idx.add_document(1, fields(&[("x", "hello")]));
        cat.save_vertex(1, "", r#"{"label":"A"}"#).unwrap();
        cat.save_edge(1, 1, 2, "link", "{}").unwrap();
        vec_idx.add(1, vec![1.0, 2.0]);
        cat.save_column_stats("t", "x", 5, 0, Some("1"), Some("10"), 20)
            .unwrap();
        cat.save_scoring_params("bm25", r#"{"alpha":1.5}"#).unwrap();

        lengths_before = idx.get_doc_length(1, "x");
        posting_count_before = idx.get_posting_list("x", "hello").len();
    }

    let (conn2, cat2) = open_catalog(&path);
    assert_eq!(cat2.load_tables().unwrap().len(), 1);
    let store2 = document_store(conn2.clone(), "t");
    assert_eq!(store2.len(), 1);
    assert_eq!(cat2.load_vertices().unwrap().len(), 1);
    assert_eq!(cat2.load_edges().unwrap().len(), 1);
    let vec2 = vector_index(conn2.clone(), "t", "emb", 2);
    assert_eq!(vec2.count(), 1);
    let idx2 = inverted_index(conn2, "t");
    assert_eq!(
        idx2.get_posting_list("x", "hello").len(),
        posting_count_before
    );
    assert_eq!(idx2.get_doc_length(1, "x"), lengths_before);
    assert_eq!(cat2.load_column_stats("t").unwrap().len(), 1);
    assert_eq!(
        cat2.load_scoring_params("bm25").unwrap().as_deref(),
        Some(r#"{"alpha":1.5}"#)
    );
}
