//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::blob::decode_f64_tensor_blob;
use super::*;
use crate::sqlite::catalog::Catalog;
use crate::StorageBackendError;
use uqa_core::Value;

fn store() -> SQLiteDocumentStore {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat = Catalog::open(mc.clone()).unwrap();
    SQLiteDocumentStore::new(mc, "articles")
}

fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn put_get_round_trip() {
    let mut s = store();
    s.put(1, doc([("title", Value::Str("rust".into()))]))
        .unwrap();
    let got = s.get(1).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
}

#[test]
fn typed_lists_round_trip_without_becoming_bytes_or_floats() {
    let mut s = store();
    let expected = doc([
        (
            "short_ints",
            Value::List(vec![Value::Int(10), Value::Int(20)]),
        ),
        ("empty", Value::List(Vec::new())),
        (
            "matrix",
            Value::List(vec![
                Value::List(vec![Value::Int(1), Value::Int(2)]),
                Value::List(vec![Value::Int(3), Value::Int(4)]),
            ]),
        ),
        (
            "nested_map",
            Value::Map(BTreeMap::from([(
                "flags".into(),
                Value::List(vec![Value::Int(0), Value::Int(1)]),
            )])),
        ),
        ("long_ints", Value::List((0..64).map(Value::Int).collect())),
        ("bytes", Value::Bytes(vec![1, 2, 3])),
        ("fixed_char", Value::FixedChar("x   ".into())),
        ("json", Value::Json("{\"b\":2,\"a\":1}".into())),
        ("jsonb", Value::JsonB("{\"a\": 1, \"b\": 2}".into())),
    ]);
    s.put(2, expected.clone()).unwrap();

    let restored = s.get(2).unwrap().unwrap();
    assert_eq!(restored, expected);
    assert_eq!(
        s.get_field(2, "short_ints").unwrap(),
        Some(Value::List(vec![Value::Int(10), Value::Int(20)]))
    );
    assert_eq!(
        s.get_fields_bulk(&[2], "empty").unwrap().get(&2),
        Some(&Value::List(Vec::new()))
    );
    assert_eq!(s.get_many(&[2]).unwrap().get(&2), Some(&expected));

    s.conn
        .with(|connection| {
            let body: String = connection.query_row(
                "SELECT body FROM _documents
                 WHERE table_name = 'articles' AND doc_id = 2",
                [],
                |row| row.get(0),
            )?;
            assert!(body.contains(VALUE_BLOB_TYPED_JSON), "{body}");
            Ok(())
        })
        .unwrap();
}

#[test]
fn user_maps_that_resemble_internal_blob_markers_round_trip_as_data() {
    let mut s = store();
    let expected = doc([
        ("bytes_marker", blob_marker("other_field".into())),
        (
            "value_marker",
            value_blob_marker("other_field".into(), VALUE_BLOB_F64_LIST),
        ),
    ]);
    s.put(4, expected.clone()).unwrap();

    assert_eq!(s.get(4).unwrap(), Some(expected.clone()));
    assert_eq!(
        s.get_field(4, "bytes_marker").unwrap(),
        expected.get("bytes_marker").cloned()
    );
    assert_eq!(
        s.get_fields_multi(&[4], &["bytes_marker", "value_marker"])
            .unwrap()
            .get(&4),
        Some(&vec![
            expected["bytes_marker"].clone(),
            expected["value_marker"].clone(),
        ])
    );
}

#[test]
fn patch_fields_preserves_ambiguous_list_variants() {
    let mut s = store();
    s.put(3, doc([("value", Value::Str("old".into()))]))
        .unwrap();
    let updates = BTreeMap::from([(
        "value".to_string(),
        Value::List(vec![Value::Int(1), Value::Int(2)]),
    )]);
    assert!(s.patch_fields(3, &updates).unwrap());
    assert_eq!(
        s.get_field(3, "value").unwrap(),
        Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn placeholder_builder_reports_size_and_index_overflow() {
    assert!(doc_id_in_placeholders(1, usize::MAX).is_err());
    assert!(doc_id_in_placeholders(usize::MAX, 2).is_err());
}

#[test]
fn document_id_larger_than_sqlite_integer_is_rejected() {
    let mut s = store();
    let error = s.put(DocId::MAX, Document::new()).unwrap_err();
    assert!(matches!(
        error,
        StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message))
            if message.contains("exceeds SQLite INTEGER")
    ));
    assert_eq!(s.len().unwrap(), 0, "failed write must not insert a row");

    assert!(matches!(
        s.get(DocId::MAX),
        Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
            if message.contains("exceeds SQLite INTEGER")
    ));
    assert!(matches!(
        s.get_many(&[DocId::MAX]),
        Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
            if message.contains("exceeds SQLite INTEGER")
    ));
}

#[test]
fn negative_persisted_document_id_is_reported_as_corruption() {
    let s = store();
    s.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _documents (table_name, doc_id, body) VALUES (?1, ?2, ?3)",
                params!["articles", -1_i64, "{}"],
            )?;
            Ok(())
        })
        .unwrap();

    assert!(matches!(
        s.doc_ids(),
        Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
            if message.contains("negative SQLite document id -1")
    ));
    assert!(matches!(
        s.next_doc_id(None),
        Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
            if message.contains("negative SQLite document id -1")
    ));
    assert!(matches!(
        s.max_doc_id(),
        Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
            if message.contains("negative SQLite document id -1")
    ));
}

/// A real storage write failure (`SQLITE_FULL` via `max_page_count`)
/// must surface as `Err`, never as a silently-dropped write: callers
/// use this signal to abort the enclosing statement or transaction.
#[test]
fn put_failure_is_reported_not_swallowed() {
    let mut s = store();
    s.put(1, doc([("title", Value::Str("small".into()))]))
        .unwrap();
    s.conn
        .with(|c| {
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            c.pragma_update(None, "max_page_count", pages)?;
            Ok(())
        })
        .unwrap();
    let huge = "x".repeat(8 * 1024 * 1024);
    let err = s.put(2, doc([("body", Value::Str(huge))]));
    assert!(
        err.is_err(),
        "oversized put must fail once the page budget is exhausted"
    );
    // The failure must not have corrupted existing data.
    let got = s.get(1).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&Value::Str("small".into())));
}

#[test]
fn delete_removes_row() {
    let mut s = store();
    s.put(1, doc([("a", Value::Int(1))])).unwrap();
    s.delete(1).unwrap();
    assert!(s.get(1).unwrap().is_none());
    assert_eq!(s.len().unwrap(), 0);
}

#[test]
fn doc_ids_sorted_ascending() {
    let mut s = store();
    s.put(3, Document::new()).unwrap();
    s.put(1, Document::new()).unwrap();
    s.put(2, Document::new()).unwrap();
    assert_eq!(s.doc_ids().unwrap(), vec![1, 2, 3]);
}

#[test]
fn get_field_reads_individual_field() {
    let mut s = store();
    s.put(
        7,
        doc([("year", Value::Int(2026)), ("flag", Value::Bool(true))]),
    )
    .unwrap();
    assert_eq!(s.get_field(7, "year").unwrap(), Some(Value::Int(2026)));
    assert_eq!(s.get_field(7, "flag").unwrap(), Some(Value::Bool(true)));
    assert_eq!(s.get_field(7, "missing").unwrap(), None);
}

#[test]
fn get_fields_bulk_reads_values_and_missing_as_null() {
    let mut s = store();
    s.put(
        1,
        doc([
            ("title", Value::Str("rust".into())),
            ("payload", Value::Bytes(vec![1, 2, 3])),
        ]),
    )
    .unwrap();
    s.put(2, doc([("title", Value::Str("sqlite".into()))]))
        .unwrap();

    let titles = s.get_fields_bulk(&[1, 2, 99], "title").unwrap();
    assert_eq!(titles.get(&1), Some(&Value::Str("rust".into())));
    assert_eq!(titles.get(&2), Some(&Value::Str("sqlite".into())));
    assert_eq!(titles.get(&99), Some(&Value::Null));

    let payloads = s.get_fields_bulk(&[1, 2], "payload").unwrap();
    assert_eq!(payloads.get(&1), Some(&Value::Bytes(vec![1, 2, 3])));
    assert_eq!(payloads.get(&2), Some(&Value::Null));
}

#[test]
fn find_doc_id_by_field_uses_top_level_value() {
    let mut s = store();
    s.put(
        5,
        doc([
            ("public_id", Value::Str("m-5".into())),
            ("content", Value::Str("old".into())),
        ]),
    )
    .unwrap();
    s.put(
        9,
        doc([
            ("public_id", Value::Str("m-9".into())),
            ("content", Value::Str("target".into())),
        ]),
    )
    .unwrap();

    assert_eq!(
        s.find_doc_id_by_field("public_id", &Value::Str("m-9".into()))
            .unwrap(),
        Some(9)
    );
    assert_eq!(
        s.find_doc_id_by_field("public_id", &Value::Str("missing".into()))
            .unwrap(),
        None
    );
}

#[test]
fn patch_fields_updates_body_without_losing_unmodified_values() {
    let mut s = store();
    s.put(
        31,
        doc([
            ("public_id", Value::Str("m-31".into())),
            ("content", Value::Str("old".into())),
            (
                "embedding",
                Value::List(vec![Value::Float(0.25), Value::Float(0.75)]),
            ),
            ("token_count", Value::Int(2)),
        ]),
    )
    .unwrap();

    let updates = BTreeMap::from([
        ("content".to_string(), Value::Str("new".into())),
        ("token_count".to_string(), Value::Null),
    ]);
    assert!(s.patch_fields(31, &updates).unwrap());

    let got = s.get(31).unwrap().unwrap();
    assert_eq!(got.get("public_id"), Some(&Value::Str("m-31".into())));
    assert_eq!(got.get("content"), Some(&Value::Str("new".into())));
    assert_eq!(
        got.get("embedding"),
        Some(&Value::List(vec![Value::Float(0.25), Value::Float(0.75)]))
    );
    assert!(!got.contains_key("token_count"));
}

#[test]
fn patch_fields_updates_blob_storage() {
    let mut s = store();
    s.put(
        41,
        doc([
            ("public_id", Value::Str("m-41".into())),
            ("bytes", Value::Bytes(vec![1, 2, 3])),
        ]),
    )
    .unwrap();

    let updates = BTreeMap::from([("bytes".to_string(), Value::Bytes(vec![4, 5]))]);
    assert!(s.patch_fields(41, &updates).unwrap());
    assert_eq!(
        s.get_field(41, "bytes").unwrap(),
        Some(Value::Bytes(vec![4, 5]))
    );

    s.conn
        .with(|c| {
            let bytes: Vec<u8> = c.query_row(
                &format!(
                    "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'
                       AND doc_id = 41
                       AND field_name = 'bytes'"
                ),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(bytes, vec![4, 5]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn byte_values_are_stored_as_sqlite_blobs_not_json_arrays() {
    let mut s = store();
    s.put(
        11,
        doc([
            ("bytes", Value::Bytes(vec![1, 2, 3, 4])),
            ("title", Value::Str("asset".into())),
        ]),
    )
    .unwrap();

    s.conn
        .with(|c| {
            let body: String = c.query_row(
                "SELECT body FROM _documents
                 WHERE table_name = 'articles' AND doc_id = 11",
                [],
                |r| r.get(0),
            )?;
            assert!(body.contains(BLOB_MARKER_VALUE), "{body}");
            assert!(!body.contains("\"bytes\":[1,2,3,4]"), "{body}");

            let bytes: Vec<u8> = c.query_row(
                &format!(
                    "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'
                       AND doc_id = 11
                       AND field_name = 'bytes'"
                ),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(bytes, vec![1, 2, 3, 4]);
            Ok(())
        })
        .unwrap();

    assert_eq!(
        s.get_field(11, "bytes").unwrap(),
        Some(Value::Bytes(vec![1, 2, 3, 4]))
    );
    let got = s.get(11).unwrap().unwrap();
    assert_eq!(got.get("title"), Some(&Value::Str("asset".into())));
}

#[test]
fn large_numeric_values_are_stored_as_sqlite_blobs_not_json_arrays() {
    let mut s = store();
    let embedding = Value::List((0..64).map(|i| Value::Float(f64::from(i) / 64.0)).collect());
    let tensor = Value::List(
        (0..4)
            .map(|row| {
                Value::List(
                    (0..16)
                        .map(|col| Value::Float(f64::from(row * 16 + col)))
                        .collect(),
                )
            })
            .collect(),
    );

    s.put(
        12,
        doc([
            ("embedding", embedding.clone()),
            ("tensor", tensor.clone()),
            ("title", Value::Str("vector".into())),
        ]),
    )
    .unwrap();

    s.conn
        .with(|c| {
            let body: String = c.query_row(
                "SELECT body FROM _documents
                 WHERE table_name = 'articles' AND doc_id = 12",
                [],
                |r| r.get(0),
            )?;
            assert!(body.contains(VALUE_BLOB_MARKER_VALUE), "{body}");
            assert!(body.contains(VALUE_BLOB_F64_LIST), "{body}");
            assert!(body.contains(VALUE_BLOB_F64_TENSOR), "{body}");
            assert!(!body.contains("0.984375"), "{body}");
            assert!(!body.contains("63.0"), "{body}");

            let embedding_bytes: Vec<u8> = c.query_row(
                &format!(
                    "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'
                       AND doc_id = 12
                       AND field_name = 'embedding'"
                ),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(embedding_bytes.len(), 64 * std::mem::size_of::<f64>());

            let tensor_bytes: Vec<u8> = c.query_row(
                &format!(
                    "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'
                       AND doc_id = 12
                       AND field_name = 'tensor'"
                ),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(tensor_bytes.len(), 8 + 64 * std::mem::size_of::<f64>());
            Ok(())
        })
        .unwrap();

    assert_eq!(
        s.get_field(12, "embedding").unwrap(),
        Some(embedding.clone())
    );
    assert_eq!(s.get_field(12, "tensor").unwrap(), Some(tensor.clone()));

    let fields = s.get_fields_bulk(&[12, 99], "embedding").unwrap();
    assert_eq!(fields.get(&12), Some(&embedding));
    assert_eq!(fields.get(&99), Some(&Value::Null));

    let got = s.get(12).unwrap().unwrap();
    assert_eq!(got.get("embedding"), Some(&embedding));
    assert_eq!(got.get("tensor"), Some(&tensor));
    assert_eq!(got.get("title"), Some(&Value::Str("vector".into())));
}

#[test]
fn legacy_inline_byte_arrays_are_read_without_hidden_writes() {
    let s = store();
    s.conn
        .with(|c| {
            c.execute(
                "INSERT INTO _documents (table_name, doc_id, body)
                 VALUES ('articles', 21, ?1)",
                [r#"{"bytes":[9,8,7],"title":"legacy"}"#],
            )?;
            Ok(())
        })
        .unwrap();

    let got = s.get(21).unwrap().unwrap();
    assert_eq!(got.get("bytes"), Some(&Value::Bytes(vec![9, 8, 7])));
    assert_eq!(got.get("title"), Some(&Value::Str("legacy".into())));

    s.conn
        .with(|c| {
            let body: String = c.query_row(
                "SELECT body FROM _documents
                 WHERE table_name = 'articles' AND doc_id = 21",
                [],
                |r| r.get(0),
            )?;
            assert!(!body.contains(BLOB_MARKER_VALUE), "{body}");
            assert!(body.contains("\"bytes\":[9,8,7]"), "{body}");

            let blob_rows: i64 = c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'
                       AND doc_id = 21
                       AND field_name = 'bytes'"
                ),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(blob_rows, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn missing_or_malformed_blob_rows_are_reported_as_corruption() {
    let mut s = store();
    s.put(31, doc([("bytes", Value::Bytes(vec![1, 2, 3]))]))
        .unwrap();
    s.conn
        .with(|c| {
            c.execute(
                "DELETE FROM _document_blobs
                 WHERE table_name = 'articles' AND doc_id = 31",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        s.get(31),
        Err(StorageBackendError::SQLite(
            SQLiteError::CorruptDocumentBlob { .. }
        ))
    ));

    let embedding = Value::List(
        (0..64)
            .map(|value| Value::Float(f64::from(value)))
            .collect(),
    );
    s.put(32, doc([("embedding", embedding)])).unwrap();
    s.conn
        .with(|c| {
            c.execute(
                "UPDATE _document_blobs SET bytes = x'00'
                 WHERE table_name = 'articles' AND doc_id = 32",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        s.get_field(32, "embedding"),
        Err(StorageBackendError::SQLite(
            SQLiteError::CorruptDocumentBlob { .. }
        ))
    ));
}

#[test]
fn tensor_decoder_rejects_dimension_product_overflow() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(decode_f64_tensor_blob(&bytes).unwrap(), None);
}

#[test]
fn delete_and_clear_remove_blob_rows() {
    let mut s = store();
    s.put(1, doc([("bytes", Value::Bytes(vec![1]))])).unwrap();
    s.put(2, doc([("bytes", Value::Bytes(vec![2]))])).unwrap();

    s.delete(1).unwrap();
    let remaining = s
        .conn
        .with(|c| {
            Ok(c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'"
                ),
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(remaining, 1);

    s.clear().unwrap();
    let remaining = s
        .conn
        .with(|c| {
            Ok(c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = 'articles'"
                ),
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(remaining, 0);
}
