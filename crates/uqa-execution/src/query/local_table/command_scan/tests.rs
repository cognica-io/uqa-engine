//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::local_table::LocalTableScanConfig;
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_core::{CancellationToken, Value};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{DocumentMetadata, MemoryDocumentStore, StoredDocument};

struct Table {
    columns: Vec<ColumnDef>,
    documents: RwLock<Box<dyn DocumentStore>>,
}

impl crate::query::table_read::TableRead for Table {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        self.columns.clone()
    }
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.documents.read()
    }
}

fn row(a: i64, xmin: u32) -> StoredDocument {
    StoredDocument::with_metadata(
        [("a".into(), Value::Int(a))].into(),
        DocumentMetadata::with_tuple_xmin(xmin),
    )
}

fn scan(projection: &[&str]) -> LocalTableRowSource {
    let uqa_sql::Statement::CreateTable(definition) = uqa_sql::compile(
        "CREATE TABLE t (a INT, calculated INT GENERATED ALWAYS AS (a + 1) VIRTUAL)",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let mut base = MemoryDocumentStore::new();
    for (id, a) in [(1, 10), (3, 30), (5, 50), (8, 80), (u64::MAX, 90)] {
        base.put_stored(id, row(a, 41)).unwrap();
    }
    let mut private = MemoryDocumentStore::new();
    private.put_stored(3, row(300, 51)).unwrap();
    let mut changes = DocumentChanges::default()
        .with_retained(
            private.snapshot().unwrap(),
            [(1, false), (3, true), (5, false)].into(),
            &CancellationToken::new(),
        )
        .unwrap();
    changes.insert_shared(
        4,
        Some((
            Arc::new(row(400, 52).into_fields()),
            DocumentMetadata::with_tuple_xmin(52),
        )),
    );
    let columns = projection
        .iter()
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    LocalTableRowSource::new(LocalTableScanConfig {
        cancellation: CancellationToken::new(),
        serializable: None,
        table_name: "t".into(),
        table: Arc::new(Table {
            columns: definition.columns.clone(),
            documents: RwLock::new(Box::new(base)),
        }),
        column_definitions: Arc::new(definition.columns),
        columns: columns.clone(),
        schema: columns.clone(),
        physical_schema: crate::RowSchema::new(columns),
        metadata: uqa_sql::plan::source_projection::RelationMetadataProjection::default(),
        table_oid: None,
        predicate: None,
        estimated_cardinality: 4,
        lock_origin: Some(("t".into(), "public.t".into())),
        recheck_pins: None,
        command_changes: Some(changes),
    })
}

#[test]
fn command_scan_merges_selected_sources_in_identity_order_across_small_batches() {
    let mut scan = scan(&["a"]);
    assert!(scan.next_command_physical_rows_batch(0).unwrap().is_empty());
    let mut observed = Vec::new();
    loop {
        let batch = scan.next_command_physical_rows_batch(1).unwrap();
        if batch.is_empty() {
            break;
        }
        for row in batch {
            observed.push((
                row.lock_origins()[0].doc_id,
                scan.physical_schema.view(&row).to_result_row()["a"].clone(),
            ));
        }
    }
    assert_eq!(
        observed,
        vec![
            (3, Value::Int(300)),
            (4, Value::Int(400)),
            (8, Value::Int(80)),
            (u64::MAX, Value::Int(90))
        ]
    );
    assert!(scan.next_command_physical_rows_batch(2).unwrap().is_empty());
}

#[test]
fn command_scan_keeps_metadata_generated_values_and_predicate_progress() {
    let mut scan = scan(&["a", "calculated", "xmin"]);
    let expression = crate::ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Less,
        lhs: Box::new(crate::ScalarExpr::Column("a".into())),
        rhs: Box::new(crate::ScalarExpr::Literal(Value::Int(100))),
    };
    scan.predicate =
        crate::ProjectedPredicate::compile_with_schema(&expression, &scan.physical_schema, &[])
            .unwrap();
    assert!(scan.predicate.is_some());
    let first = scan.next_command_physical_rows_batch(1).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].lock_origins()[0].doc_id, 8);
    let values = scan.physical_schema.view(&first[0]).to_result_row();
    assert_eq!(values["calculated"], Value::Int(81));
    assert_eq!(values["xmin"], Value::Int(41));
    let last = scan.next_command_physical_rows_batch(2).unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].lock_origins()[0].doc_id, u64::MAX);
    scan.cancellation.cancel();
    assert!(scan.next_command_physical_rows_batch(1).is_err());
}
