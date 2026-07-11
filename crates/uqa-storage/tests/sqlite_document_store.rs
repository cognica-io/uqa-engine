//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_sqlite_document_store`.
//!
//! The Rust store persists documents in the catalog `_documents` table
//! as typed JSON rather than creating one `SQLite` column per logical
//! field. These tests assert the same public `DocumentStore`
//! behaviour through the Rust storage surface.

use uqa_core::{PathSegment, Value};
use uqa_storage::document_store::{Document, DocumentStore};
use uqa_storage::sqlite::{Catalog, ManagedConnection, SQLiteDocumentStore};

const TEST_FLOAT: f64 = 3.125;

fn make_store(table: &str) -> (tempfile::TempDir, ManagedConnection, SQLiteDocumentStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = ManagedConnection::open(&path).unwrap();
    let _catalog = Catalog::open(conn.clone()).unwrap();
    let store = SQLiteDocumentStore::new(conn.clone(), table);
    (dir, conn, store)
}

fn make_store_in_dir(
    dir: &tempfile::TempDir,
    table: &str,
) -> (ManagedConnection, SQLiteDocumentStore) {
    let path = dir.path().join("test.db");
    let conn = ManagedConnection::open(&path).unwrap();
    let _catalog = Catalog::open(conn.clone()).unwrap();
    let store = SQLiteDocumentStore::new(conn.clone(), table);
    (conn, store)
}

fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

#[test]
fn put_and_get() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([
            ("id", Value::Int(1)),
            ("name", Value::Str("alice".into())),
            ("score", Value::Float(9.5)),
            ("active", Value::Int(1)),
        ]),
    ).unwrap();
    let got = store.get(1).unwrap();
    assert_eq!(got["id"], Value::Int(1));
    assert_eq!(got["name"], Value::Str("alice".into()));
    assert_eq!(got["score"], Value::Float(9.5));
    assert_eq!(got["active"], Value::Int(1));
}

#[test]
fn get_missing_returns_none() {
    let (_dir, _conn, store) = make_store("t1");
    assert!(store.get(999).is_none());
}

#[test]
fn delete() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([("id", Value::Int(1)), ("name", Value::Str("alice".into()))]),
    ).unwrap();
    store.delete(1).unwrap();
    assert!(store.get(1).is_none());
}

#[test]
fn delete_nonexistent_is_noop() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.delete(42).unwrap();
}

#[test]
fn overwrite_same_doc_id() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([
            ("id", Value::Int(1)),
            ("name", Value::Str("alice".into())),
            ("score", Value::Float(5.0)),
        ]),
    ).unwrap();
    store.put(
        1,
        doc([
            ("id", Value::Int(1)),
            ("name", Value::Str("bob".into())),
            ("score", Value::Float(8.0)),
        ]),
    ).unwrap();
    let got = store.get(1).unwrap();
    assert_eq!(got["name"], Value::Str("bob".into()));
    assert_eq!(got["score"], Value::Float(8.0));
}

#[test]
fn integer_column() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("val", Value::Int(42))])).unwrap();
    assert_eq!(store.get_field(1, "val"), Some(Value::Int(42)));
}

#[test]
fn text_column() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("val", Value::Str("hello".into()))])).unwrap();
    assert_eq!(store.get_field(1, "val"), Some(Value::Str("hello".into())));
}

#[test]
fn real_column() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("val", Value::Float(TEST_FLOAT))])).unwrap();
    assert_eq!(store.get_field(1, "val"), Some(Value::Float(TEST_FLOAT)));
}

#[test]
fn boolean_stored_as_integer() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("val", Value::Int(1))])).unwrap();
    store.put(2, doc([("val", Value::Int(0))])).unwrap();
    assert_eq!(store.get_field(1, "val"), Some(Value::Int(1)));
    assert_eq!(store.get_field(2, "val"), Some(Value::Int(0)));
}

#[test]
fn blob_column() {
    let (_dir, _conn, mut store) = make_store("t1");
    let data = vec![0, 1, 2, 3];
    store.put(1, doc([("val", Value::Bytes(data.clone()))])).unwrap();
    assert_eq!(store.get_field(1, "val"), Some(Value::Bytes(data)));
}

#[test]
fn serial_maps_to_integer() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([("pk", Value::Int(1)), ("name", Value::Str("row1".into()))]),
    ).unwrap();
    assert_eq!(store.get_field(1, "pk"), Some(Value::Int(1)));
}

#[test]
fn missing_field_stored_as_null() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([("id", Value::Int(1)), ("name", Value::Str("alice".into()))]),
    ).unwrap();
    let got = store.get(1).unwrap();
    assert!(!got.contains_key("score"));
    assert!(!got.contains_key("active"));
}

#[test]
fn explicit_none_stored_as_null() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([
            ("id", Value::Int(1)),
            ("name", Value::Null),
            ("score", Value::Null),
            ("active", Value::Null),
        ]),
    ).unwrap();
    let got = store.get(1).unwrap();
    assert!(!got.contains_key("name"));
    assert!(!got.contains_key("score"));
}

#[test]
fn get_field_null_returns_none() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    assert_eq!(store.get_field(1, "name"), None);
}

#[test]
fn doc_ids_empty_store() {
    let (_dir, _conn, store) = make_store("t1");
    assert!(store.doc_ids().is_empty());
}

#[test]
fn doc_ids_after_inserts() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        10,
        doc([("id", Value::Int(10)), ("name", Value::Str("a".into()))]),
    ).unwrap();
    store.put(
        20,
        doc([("id", Value::Int(20)), ("name", Value::Str("b".into()))]),
    ).unwrap();
    store.put(
        30,
        doc([("id", Value::Int(30)), ("name", Value::Str("c".into()))]),
    ).unwrap();
    assert_eq!(store.doc_ids(), vec![10, 20, 30]);
}

#[test]
fn doc_ids_after_delete() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    store.put(2, doc([("id", Value::Int(2))])).unwrap();
    store.delete(1).unwrap();
    assert_eq!(store.doc_ids(), vec![2]);
}

#[test]
fn len_empty() {
    let (_dir, _conn, store) = make_store("t1");
    assert_eq!(store.len(), 0);
}

#[test]
fn len_after_inserts() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    store.put(2, doc([("id", Value::Int(2))])).unwrap();
    assert_eq!(store.len(), 2);
}

#[test]
fn len_after_delete() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    store.put(2, doc([("id", Value::Int(2))])).unwrap();
    store.delete(1).unwrap();
    assert_eq!(store.len(), 1);
}

#[test]
fn max_doc_id_empty_returns_zero() {
    let (_dir, _conn, store) = make_store("t1");
    assert_eq!(store.max_doc_id(), 0);
}

#[test]
fn max_doc_id_single_row() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(7, doc([("id", Value::Int(7))])).unwrap();
    assert_eq!(store.max_doc_id(), 7);
}

#[test]
fn max_doc_id_multiple_rows() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(3, doc([("id", Value::Int(3))])).unwrap();
    store.put(10, doc([("id", Value::Int(10))])).unwrap();
    store.put(5, doc([("id", Value::Int(5))])).unwrap();
    assert_eq!(store.max_doc_id(), 10);
}

#[test]
fn max_doc_id_after_deleting_max() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    store.put(5, doc([("id", Value::Int(5))])).unwrap();
    store.delete(5).unwrap();
    assert_eq!(store.max_doc_id(), 1);
}

#[test]
fn get_field_existing_field() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([
            ("id", Value::Int(1)),
            ("name", Value::Str("alice".into())),
            ("score", Value::Float(8.0)),
        ]),
    ).unwrap();
    assert_eq!(store.get_field(1, "name"), Some(Value::Str("alice".into())));
    assert_eq!(store.get_field(1, "score"), Some(Value::Float(8.0)));
}

#[test]
fn get_field_missing_doc() {
    let (_dir, _conn, store) = make_store("t1");
    assert_eq!(store.get_field(99, "name"), None);
}

#[test]
fn get_field_unknown_column_returns_none() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("id", Value::Int(1))])).unwrap();
    assert_eq!(store.get_field(1, "nonexistent"), None);
}

#[test]
fn eval_path_flat_single_key() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([("id", Value::Int(1)), ("name", Value::Str("alice".into()))]),
    ).unwrap();
    assert_eq!(
        store.eval_path(1, &[PathSegment::Key("name".into())]),
        Some(Value::Str("alice".into()))
    );
}

#[test]
fn eval_path_flat_missing_doc() {
    let (_dir, _conn, store) = make_store("t1");
    assert_eq!(
        store.eval_path(99, &[PathSegment::Key("name".into())]),
        None
    );
}

#[test]
fn eval_path_nested_path_falls_back_to_dict_traversal() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, doc([("name", Value::Str("alice".into()))])).unwrap();
    assert_eq!(
        store.eval_path(
            1,
            &[
                PathSegment::Key("name".into()),
                PathSegment::Key("first".into())
            ]
        ),
        None
    );
}

#[test]
fn eval_path_single_element_path_uses_get_field() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(
        1,
        doc([("id", Value::Int(1)), ("score", Value::Float(7.5))]),
    ).unwrap();
    assert_eq!(
        store.eval_path(1, &[PathSegment::Key("score".into())]),
        Some(Value::Float(7.5))
    );
}

#[test]
fn two_tables_independent() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, mut store_a) = make_store_in_dir(&dir, "alpha");
    let mut store_b = SQLiteDocumentStore::new(conn, "beta");

    store_a.put(
        1,
        doc([("x", Value::Int(10)), ("y", Value::Str("a".into()))]),
    ).unwrap();
    store_b.put(
        1,
        doc([
            ("p", Value::Float(TEST_FLOAT)),
            ("q", Value::Str("pi".into())),
        ]),
    ).unwrap();

    assert_eq!(store_a.len(), 1);
    assert_eq!(store_b.len(), 1);
    assert_eq!(store_a.get(1).unwrap()["x"], Value::Int(10));
    assert_eq!(store_b.get(1).unwrap()["p"], Value::Float(TEST_FLOAT));
    store_a.delete(1).unwrap();
    assert_eq!(store_a.len(), 0);
    assert_eq!(store_b.len(), 1);
}

#[test]
fn different_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, mut store_1) = make_store_in_dir(&dir, "narrow");
    let mut store_2 = SQLiteDocumentStore::new(conn, "wide");

    store_1.put(1, doc([("val", Value::Int(100))])).unwrap();
    store_2.put(
        1,
        doc([
            ("a", Value::Str("x".into())),
            ("b", Value::Str("y".into())),
            ("c", Value::Float(1.0)),
            ("d", Value::Int(0)),
        ]),
    ).unwrap();

    assert_eq!(store_1.get_field(1, "val"), Some(Value::Int(100)));
    assert_eq!(store_2.get_field(1, "c"), Some(Value::Float(1.0)));
    assert_eq!(store_1.doc_ids(), vec![1]);
    assert_eq!(store_2.doc_ids(), vec![1]);
}

#[test]
fn empty_document() {
    let (_dir, _conn, mut store) = make_store("t1");
    store.put(1, Document::new()).unwrap();
    assert_eq!(store.get(1), Some(Document::new()));
}

#[test]
fn large_doc_id() {
    let (_dir, _conn, mut store) = make_store("t1");
    let big_id = 1_u64 << 40;
    store.put(
        big_id,
        doc([
            ("id", Value::Int(big_id as i64)),
            ("name", Value::Str("big".into())),
        ]),
    ).unwrap();
    let got = store.get(big_id).unwrap();
    assert_eq!(got["id"], Value::Int(big_id as i64));
    assert_eq!(got["name"], Value::Str("big".into()));
    assert_eq!(store.max_doc_id(), big_id);
}

#[test]
fn idempotent_table_creation() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, mut store1) = make_store_in_dir(&dir, "dup");
    store1.put(1, doc([("val", Value::Int(42))])).unwrap();
    let store2 = SQLiteDocumentStore::new(conn, "dup");
    assert_eq!(store2.get(1).unwrap()["val"], Value::Int(42));
}
