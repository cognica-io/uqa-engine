//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{ScoredDocumentSource, ScoredInput};
use crate::{query::table_read::TableRead, row_locks::recheck::RecheckDoc, RowSource};
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::ast::{ColumnDef, Statement};
use uqa_storage::{DocumentMetadata, DocumentStore, MemoryDocumentStore, StoredDocument};

struct Table {
    definitions: Vec<ColumnDef>,
    documents: RwLock<Box<dyn DocumentStore>>,
}

impl TableRead for Table {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        self.definitions.clone()
    }

    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.documents.read()
    }

    fn maps_integer_keys(&self) -> bool {
        false
    }
}

fn source(declaration: &str, fields: &[&str]) -> ScoredDocumentSource {
    let Statement::CreateTable(table) = uqa_sql::compile(declaration).unwrap().remove(0) else {
        panic!("table declaration");
    };
    let mut documents = MemoryDocumentStore::new();
    documents
        .put_stored(
            7,
            StoredDocument::with_metadata(
                [
                    ("value".into(), Value::Int(11)),
                    ("xmin".into(), Value::Int(91)),
                    ("_doc_id".into(), Value::Int(81)),
                ]
                .into(),
                DocumentMetadata::with_tuple_xmin(42),
            ),
        )
        .unwrap();
    ScoredDocumentSource::new(
        "t",
        Arc::new(Table {
            definitions: table.columns,
            documents: RwLock::new(Box::new(documents)),
        }),
        ScoredInput::All,
        fields.iter().map(|field| (*field).into()).collect(),
        None,
        None,
    )
}

#[test]
fn prepared_projection_preserves_selected_generated_and_tuple_values() {
    let declaration = "CREATE TABLE t (value integer, computed integer GENERATED ALWAYS AS (value + 1) VIRTUAL, failing integer GENERATED ALWAYS AS (value / 0) VIRTUAL)";
    for (fields, expected, batched) in [
        (vec!["value"], vec![Value::Int(11)], true),
        (vec!["computed"], vec![Value::Int(12)], false),
        (vec!["xmin"], vec![Value::Int(42)], false),
        (
            vec!["computed", "value", "xmin"],
            vec![Value::Int(12), Value::Int(11), Value::Int(42)],
            false,
        ),
    ] {
        let mut source = source(declaration, &fields);
        assert_eq!(source.supports_batched_consumption(), batched);
        let rows = source.next_physical_batch(8).unwrap();
        assert_eq!(rows.len(), 1);
        for (slot, value) in expected.iter().enumerate() {
            assert_eq!(rows[0].value(slot), Some(value), "{fields:?}");
        }
        assert!(source.next_physical_batch(8).unwrap().is_empty());
    }
    let mut failing = source(declaration, &["failing"]);
    assert!(!failing.supports_batched_consumption());
    assert!(matches!(
        failing.next_physical_batch(8),
        Err(crate::ExecError::SQL(error)) if error.sqlstate() == Some("22012")
    ));
}

#[test]
fn prepared_projection_preserves_declared_metadata_and_repeated_slots() {
    for (declaration, expected_xmin, expected_doc_id, batched) in [
        ("CREATE TABLE t (value integer)", 42, 7, false),
        (
            "CREATE TABLE t (value integer, xmin integer, _doc_id integer)",
            91,
            81,
            true,
        ),
    ] {
        let mut source = source(
            declaration,
            &["value", "_doc_id", "xmin", "value", "missing"],
        );
        assert_eq!(source.supports_batched_consumption(), batched);
        let rows = source.next_physical_batch(8).unwrap();
        assert_eq!(rows.len(), 1);
        for (slot, value) in [
            Value::Int(11),
            Value::Int(expected_doc_id),
            Value::Int(expected_xmin),
            Value::Int(11),
            Value::Null,
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(rows[0].value(slot), Some(value));
        }
    }
}

#[test]
fn prepared_projection_keeps_lock_and_recheck_decisions_dynamic() {
    let original = source("CREATE TABLE t (value integer)", &["value"]);
    assert!(original.supports_batched_consumption());
    let mut locked = original.with_lock_origin(Some(("t".into(), "public.t".into())));
    assert!(!locked.supports_batched_consumption());
    let rows = locked.next_physical_batch(8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value(0), Some(&Value::Int(11)));
    assert_eq!(rows[0].lock_origins()[0].doc_id, 7);
    assert!(locked.with_lock_origin(None).supports_batched_consumption());

    let original = source("CREATE TABLE t (value integer)", &["value"]);
    let mut pinned = original
        .with_recheck_pins(Some(Arc::new(vec![RecheckDoc {
            doc_id: 7,
            document: Some(Arc::new(StoredDocument::new(
                [("value".into(), Value::Int(23))].into(),
            ))),
        }])))
        .unwrap();
    assert!(!pinned.supports_batched_consumption());
    let rows = pinned.next_physical_batch(8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value(0), Some(&Value::Int(23)));
    assert!(pinned.next_physical_batch(8).unwrap().is_empty());
}

#[test]
fn prepared_projection_keeps_retrieval_input_decisions_dynamic() {
    let original = source("CREATE TABLE t (value integer)", &["value"]);
    assert!(original.supports_batched_consumption());
    let mut selected = original.with_retrieval_entries(vec![uqa_core::ScoredEntry {
        doc_id: 7,
        score: 0.25,
    }]);
    assert!(!selected.supports_batched_consumption());
    let rows = selected.next_physical_batch(8).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value(0), Some(&Value::Int(11)));
    assert!(selected.next_physical_batch(8).unwrap().is_empty());
}
