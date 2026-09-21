//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::{RwLock, RwLockReadGuard};
use uqa_core::{Payload, PostingEntry};
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
fn primary_key_mapping_requires_an_integer_column_and_nonnegative_integer_value() {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t (id INTEGER PRIMARY KEY, other INTEGER, word TEXT)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    assert_eq!(
        primary_key_doc_id(&table.columns, "id", &Value::Int(0)),
        Some(0)
    );
    assert_eq!(
        primary_key_doc_id(&table.columns, "id", &Value::Int(19)),
        Some(19)
    );
    for (column, value) in [
        ("id", Value::Int(-1)),
        ("id", Value::Float(19.0)),
        ("id", Value::Null),
        ("other", Value::Int(19)),
        ("word", Value::Int(19)),
        ("missing", Value::Int(19)),
    ] {
        assert_eq!(primary_key_doc_id(&table.columns, column, &value), None);
    }
    let uqa_sql::Statement::CreateTable(text_table) =
        uqa_sql::compile("CREATE TABLE words (word TEXT PRIMARY KEY)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    assert_eq!(
        primary_key_doc_id(&text_table.columns, "word", &Value::Int(19)),
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
