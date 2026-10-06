//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ColumnValueIndex;
use parking_lot::Mutex;
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_core::{Payload, PostingEntry};
use uqa_storage::StorageBackendResult;
use uqa_storage::{DocumentStore, MemoryDocumentStore};

struct Table {
    documents: RwLock<Box<dyn DocumentStore>>,
}

impl Table {
    fn new(rows: impl IntoIterator<Item = (DocId, Vec<(&'static str, Value)>)>) -> Self {
        let mut documents = MemoryDocumentStore::new();
        for (id, fields) in rows {
            documents
                .put(
                    id,
                    fields
                        .into_iter()
                        .map(|(name, value)| (name.into(), value))
                        .collect(),
                )
                .unwrap();
        }
        Self {
            documents: RwLock::new(Box::new(documents)),
        }
    }
}

impl TableRead for Table {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        Vec::new()
    }

    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.documents.read()
    }

    fn maps_integer_keys(&self) -> bool {
        true
    }
}

fn postings(ids: &[DocId]) -> PostingList {
    PostingList::from_unsorted(
        ids.iter()
            .map(|id| PostingEntry::new(*id, Payload::default()))
            .collect(),
    )
}

fn indexed_conflict(
    table: &dyn TableRead,
    columns: &[String],
    values: &[Value],
    scan: impl FnMut(&str, &Predicate) -> Result<Option<PostingList>, SQLError>,
) -> Result<IndexConflictProbe, SQLError> {
    ExactLookup {
        table,
        overlay: &BTreeMap::new(),
        read: None,
    }
    .indexed_conflict(columns, values, scan)
}

#[test]
fn primary_key_mapping_requires_a_mapped_integer_column_and_a_key_that_names_an_identity() {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t (id INTEGER PRIMARY KEY, other INTEGER, word TEXT)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let limit = uqa_sql::semantics::key_identity::KEY_IDENTITY_LIMIT;
    assert_eq!(
        primary_key_doc_id(true, &table.columns, "id", &Value::Int(0)),
        Some(0)
    );
    assert_eq!(
        primary_key_doc_id(true, &table.columns, "id", &Value::Int(19)),
        Some(19)
    );
    assert_eq!(
        primary_key_doc_id(
            true,
            &table.columns,
            "id",
            &Value::Int(i64::try_from(limit - 1).unwrap())
        ),
        Some(limit - 1)
    );
    // A table written before keys named identities resolves every key through its index.
    assert_eq!(
        primary_key_doc_id(false, &table.columns, "id", &Value::Int(19)),
        None
    );
    for (column, value) in [
        ("id", Value::Int(-1)),
        ("id", Value::Int(i64::try_from(limit).unwrap())),
        ("id", Value::Int(i64::MAX)),
        ("id", Value::Float(19.0)),
        ("id", Value::Null),
        ("other", Value::Int(19)),
        ("word", Value::Int(19)),
        ("missing", Value::Int(19)),
    ] {
        assert_eq!(
            primary_key_doc_id(true, &table.columns, column, &value),
            None
        );
    }
    let uqa_sql::Statement::CreateTable(text_table) =
        uqa_sql::compile("CREATE TABLE words (word TEXT PRIMARY KEY)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    assert_eq!(
        primary_key_doc_id(true, &text_table.columns, "word", &Value::Int(19)),
        None
    );
}

#[test]
fn first_usable_index_verifies_other_fields_after_index_selection() {
    let table = Table::new([
        (
            1,
            vec![("a", Value::Int(9)), ("b", Value::Str("key".into()))],
        ),
        (
            2,
            vec![("a", Value::Int(7)), ("b", Value::Str("key".into()))],
        ),
    ]);
    let mut visited = Vec::new();
    let found = indexed_conflict(
        &table,
        &["a".into(), "b".into()],
        &[Value::Int(7), Value::Str("key".into())],
        |column, predicate| {
            assert!(table.documents.try_write().is_some());
            visited.push(column.to_string());
            if column == "a" {
                return Ok(None);
            }
            assert_eq!(predicate, &Predicate::Equals(Value::Str("key".into())));
            Ok(Some(postings(&[1, 2])))
        },
    )
    .unwrap();
    assert_eq!(visited, ["a", "b"]);
    assert_eq!(found, IndexConflictProbe::Conflict(2));
}

#[test]
fn an_answerable_empty_index_never_falls_back_to_other_columns() {
    let table = Table::new([(1, vec![("b", Value::Int(7))])]);
    let found = indexed_conflict(
        &table,
        &["a".into(), "b".into()],
        &[Value::Null, Value::Int(7)],
        |column, predicate| {
            assert_eq!(column, "a");
            assert_eq!(predicate, &Predicate::IsNull);
            Ok(Some(PostingList::new()))
        },
    )
    .unwrap();
    assert_eq!(found, IndexConflictProbe::NoConflict);
}

#[test]
fn composite_verification_keeps_missing_as_null_and_structural_value_equality() {
    let table = Table::new([
        (1, vec![("a", Value::Int(7)), ("b", Value::Int(2))]),
        (2, vec![("a", Value::Int(7))]),
    ]);
    for (value, expected) in [
        (Value::Null, IndexConflictProbe::Conflict(2)),
        (Value::Int(2), IndexConflictProbe::Conflict(1)),
        (Value::Str("2".into()), IndexConflictProbe::NoConflict),
    ] {
        assert_eq!(
            indexed_conflict(
                &table,
                &["a".into(), "b".into()],
                &[Value::Int(7), value],
                |_, _| Ok(Some(postings(&[1, 2]))),
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn unanswerable_probes_and_index_failures_remain_distinct() {
    let table = Table::new([]);
    let columns = ["a".into()];
    let values = [Value::Int(7)];
    assert_eq!(
        indexed_conflict(&table, &columns, &values, |_, _| Ok(None)).unwrap(),
        IndexConflictProbe::Unanswerable
    );
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    let error = indexed_conflict(&table, &columns, &values, |_, _| {
        Err(SQLError::from(cancellation.check().unwrap_err()))
    })
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn private_rows_replace_delete_and_supply_matches_without_hiding_later_candidates() {
    let table = Table::new([
        (1, vec![("a", Value::Int(7))]),
        (2, vec![("a", Value::Int(7))]),
    ]);
    let overlay = BTreeMap::from([
        (1, None),
        (
            3,
            Some(StoredDocument::new(BTreeMap::from([(
                "a".into(),
                Value::Int(9),
            )]))),
        ),
    ]);
    let lookup = ExactLookup {
        table: &table,
        overlay: &overlay,
        read: None,
    };
    for indexed in [false, true] {
        for (value, expected) in [(7, Some(2)), (9, Some(3)), (10, None)] {
            assert_eq!(
                lookup
                    .find_conflict(&[], &["a".into()], &[Value::Int(value)], |_, _| {
                        Ok(indexed.then(|| {
                            if value == 7 {
                                postings(&[1, 2])
                            } else {
                                PostingList::new()
                            }
                        }))
                    })
                    .unwrap(),
                expected
            );
        }
    }
    assert_eq!(lookup.find_field("a", &Value::Int(7)).unwrap(), Some(2));
    assert_eq!(lookup.find_field("a", &Value::Int(9)).unwrap(), Some(3));
}

#[test]
fn single_field_null_distinguishes_absent_fields_with_and_without_private_rows() {
    let table = Table::new([(1, vec![]), (2, vec![("a", Value::Null)])]);
    for overlay in [
        BTreeMap::new(),
        BTreeMap::from([(3, Some(StoredDocument::new(BTreeMap::new())))]),
    ] {
        let lookup = ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None,
        };
        assert_eq!(lookup.find_field("a", &Value::Null).unwrap(), Some(2));
        assert_eq!(
            lookup
                .find_conflict(&[], &["a".into()], &[Value::Null], |_, _| Ok(None))
                .unwrap(),
            Some(if overlay.is_empty() { 1 } else { 3 })
        );
    }
}

#[test]
fn malformed_conflict_keys_do_not_read_an_index_or_document() {
    let table = Table::new([]);
    let overlay = BTreeMap::new();
    let lookup = ExactLookup {
        table: &table,
        overlay: &overlay,
        read: None,
    };
    for columns in [vec![], vec!["a".into()]] {
        assert_eq!(
            lookup
                .find_conflict(&[], &columns, &[], |_, _| panic!(
                    "malformed probe read an index"
                ))
                .unwrap(),
            None
        );
    }
}

/// An index-backed read may fetch individual fields, but must not enumerate or decode rows.
struct FieldOnlyStore {
    fields: BTreeMap<DocId, Value>,
    reads: Arc<Mutex<Vec<DocId>>>,
}

impl DocumentStore for FieldOnlyStore {
    fn get_field(&self, id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        assert_eq!(field, "a");
        self.reads.lock().push(id);
        Ok(self.fields.get(&id).cloned())
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("field probe decoded a whole document")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("indexed field probe enumerated stored rows")
    }
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn len(&self) -> StorageBackendResult<usize> {
        unreachable!()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        unreachable!()
    }
}

#[test]
fn indexed_field_reads_mask_candidates_without_reading_stored_rows() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let table = Table {
        documents: RwLock::new(Box::new(FieldOnlyStore {
            fields: BTreeMap::new(),
            reads: reads.clone(),
        })),
    };
    let index = ColumnValueIndex::build("a", (1..=4096).map(|id| (id, Value::Int(7))));
    let mut overlay: BTreeMap<_, _> = (1..4096).map(|id| (id, None)).collect();
    overlay.insert(
        2,
        Some(StoredDocument::new(BTreeMap::from([(
            "a".into(),
            Value::Int(8),
        )]))),
    );
    let lookup = ExactLookup {
        table: &table,
        overlay: &overlay,
        read: None,
    };
    for (value, expected) in [(7, Some(4096)), (8, Some(2)), (99, None)] {
        assert_eq!(
            lookup
                .find_field_with_index("a", &Value::Int(value), |field, value| {
                    assert_eq!(field, "a");
                    assert!(
                        table.documents.try_write().is_some(),
                        "index hydration held the document guard"
                    );
                    Ok(index.field_candidates(value))
                })
                .unwrap(),
            expected
        );
    }
    assert!(reads.lock().is_empty());
    overlay.insert(
        5000,
        Some(StoredDocument::new(BTreeMap::from([(
            "a".into(),
            Value::Int(7),
        )]))),
    );
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &Value::Int(7), |_, _| panic!(
            "private match must win before index hydration"
        ))
        .unwrap(),
        Some(5000)
    );
}

#[test]
fn unchanged_field_views_keep_direct_first_match_without_index_hydration() {
    let table = Table::new([
        (1, vec![("a", Value::Int(7))]),
        (2, vec![("a", Value::Int(7))]),
    ]);
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &BTreeMap::new(),
            read: None
        }
        .find_field_with_index("a", &Value::Int(7), |_, _| panic!(
            "an unchanged view must keep its direct store lookup"
        ))
        .unwrap(),
        Some(1)
    );
}

#[test]
fn indexed_null_field_reads_recheck_presence_only_after_masking() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let table = Table {
        documents: RwLock::new(Box::new(FieldOnlyStore {
            fields: BTreeMap::from([(1, Value::Null), (3, Value::Null)]),
            reads: reads.clone(),
        })),
    };
    let index = ColumnValueIndex::build("a", (1..=3).map(|id| (id, Value::Null)));
    let overlay = BTreeMap::from([(1, None)]);
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &Value::Null, |_, value| Ok(
            index.field_candidates(value)
        ))
        .unwrap(),
        Some(3)
    );
    assert_eq!(*reads.lock(), [2, 3]);
}

#[test]
fn indexed_field_fallback_masks_invalid_values_before_comparing_visible_rows() {
    let invalid = Value::LegacyVector(
        uqa_core::LegacyVectorValue::try_from_array(
            uqa_core::LegacyVectorKind::Oid,
            uqa_core::ArrayValue::with_lower_bounds(vec![], vec![]).unwrap(),
        )
        .unwrap(),
    );
    let valid = uqa_sql::expr::cast_value(&Value::Str("2".into()), "oidvector").unwrap();
    let table = Table::new([
        (1, vec![("a", invalid.clone())]),
        (2, vec![("a", valid.clone())]),
    ]);
    let index =
        ColumnValueIndex::build("a", [(1, invalid.clone()), (2, valid.clone())].into_iter());
    let mut overlay = BTreeMap::from([(1, None)]);
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &valid, |_, value| Ok(index.field_candidates(value)))
        .unwrap(),
        Some(2)
    );
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &invalid, |_, _| panic!(
            "an invalid query must not hydrate an index"
        ))
        .unwrap_err()
        .sqlstate(),
        Some("42804")
    );
    overlay.remove(&1);
    overlay.insert(3, None);
    assert_eq!(
        ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &valid, |_, value| Ok(index.field_candidates(value)))
        .unwrap_err()
        .sqlstate(),
        Some("42804")
    );
}

#[test]
fn indexed_raw_field_probes_preserve_typed_enum_errors_and_record_row_equality() {
    let label = |oid| {
        Value::Enum(uqa_core::EnumValue::new(
            oid,
            uqa_core::EnumLabelKey::from_bytes(vec![128]).unwrap(),
        ))
    };
    let table = Table::new([(1, vec![("a", label(10))])]);
    let index = ColumnValueIndex::build("a", [(1, label(10))].into_iter());
    let overlay = BTreeMap::from([(2, None)]);
    for value in [label(20), Value::Int(7)] {
        assert!(ExactLookup {
            table: &table,
            overlay: &overlay,
            read: None
        }
        .find_field_with_index("a", &value, |_, value| Ok(index.field_candidates(value)))
        .unwrap_err()
        .to_string()
        .contains("enum comparison reached operands of different types"));
    }
    let record = Value::Record(vec![("x".into(), Value::Int(1))]);
    let row = Value::Row(vec![Value::Int(1)].into());
    for wrap in [
        (|value| value) as fn(Value) -> Value,
        |value| Value::List(vec![value]),
        |value| Value::Row(vec![value].into()),
        |value| {
            Value::Array(uqa_core::ArrayValue::with_lower_bounds(vec![value], vec![1]).unwrap())
        },
    ] {
        for (stored, requested) in [(record.clone(), row.clone()), (row.clone(), record.clone())] {
            let (stored, requested) = (wrap(stored), wrap(requested));
            let table = Table::new([(1, vec![("a", stored.clone())])]);
            let index = ColumnValueIndex::build("a", [(1, stored)].into_iter());
            assert_eq!(
                ExactLookup {
                    table: &table,
                    overlay: &overlay,
                    read: None
                }
                .find_field_with_index(
                    "a",
                    &requested,
                    |_, value| Ok(index.field_candidates(value))
                )
                .unwrap(),
                Some(1)
            );
        }
    }
}
