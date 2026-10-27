//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native final collection eliminates temporary ownership while retaining ordinary fallback output.

mod failures;
mod measurements;

use super::{bind_output_mode, collect_input};
use crate::query::{
    scored_input::{ScoredDocumentSource, ScoredInput},
    table_read::TableRead,
};
use crate::{ColumnSelection, PhysicalOperator, Project, ScalarExpr, TableScan};
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{DocId, Value};
use uqa_sql::{
    ast::{BinaryOp, ColumnDef},
    SQLResult,
};
use uqa_storage::{
    document_store::SharedDocumentRow, DocumentStore, MemoryDocumentStore, StorageBackendResult,
    StoredDocument,
};

#[derive(Default)]
struct Counts {
    shared: AtomicUsize,
    borrowed: AtomicUsize,
    releases: AtomicUsize,
}

enum Fault {
    None,
    Divide,
    CancelAfter(DocId, uqa_core::CancellationToken),
}

#[derive(Clone, Copy)]
enum ReadMode {
    Borrowed,
    Shared,
    Lost,
}

#[derive(Clone, Copy)]
enum Projection {
    Plain,
    Computed,
    Metadata,
}

struct Documents {
    inner: MemoryDocumentStore,
    counts: Arc<Counts>,
    borrow: bool,
    disappear: bool,
    fault: Fault,
}

impl Drop for Documents {
    fn drop(&mut self) {
        self.counts.releases.fetch_add(1, Ordering::Relaxed);
    }
}

impl DocumentStore for Documents {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("unexpected point read")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("unexpected full identity capture")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.inner.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        unreachable!()
    }
    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, SharedDocumentRow)>>> {
        let rows = self.inner.next_shared_fields(after, limit, fields)?;
        self.counts
            .shared
            .fetch_add(rows.as_ref().map_or(0, Vec::len), Ordering::Relaxed);
        Ok(rows)
    }
    fn for_each_next_shared_fields_borrowed(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> Option<usize> {
        if !self.borrow || (self.disappear && limit != 0) {
            return None;
        }
        let visited = self.inner.for_each_next_shared_fields_borrowed(
            after,
            limit,
            fields,
            &mut |id, values| {
                let keep_going = visitor(id, values);
                if let Fault::CancelAfter(at, token) = &self.fault {
                    if id == *at {
                        token.cancel();
                    }
                }
                keep_going
            },
        )?;
        self.counts.borrowed.fetch_add(visited, Ordering::Relaxed);
        Some(visited)
    }
}

struct Table(RwLock<Box<dyn DocumentStore>>);
impl TableRead for Table {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        Vec::new()
    }
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.0.read()
    }
    fn maps_integer_keys(&self) -> bool {
        true
    }
}

fn run(
    read: ReadMode,
    count: usize,
    budget: usize,
    projection: Projection,
) -> (Result<SQLResult, uqa_sql::SQLError>, Arc<Counts>) {
    let (input, counts) = input(read, count, budget, projection, Fault::None);
    let result = super::super::collect::<()>(|mode| {
        collect_input(input, bind_output_mode(None, mode, None)?)
    });
    assert_eq!(counts.releases.load(Ordering::Relaxed), 1);
    (result, counts)
}

fn input(
    read: ReadMode,
    count: usize,
    budget: usize,
    projection: Projection,
    fault: Fault,
) -> (Box<dyn PhysicalOperator>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let mut inner = MemoryDocumentStore::new();
    for id in 1..=count {
        inner
            .put(
                id as u64,
                [
                    ("id".into(), Value::Int(id as i64)),
                    (
                        "value".into(),
                        if id % 3 == 0 {
                            Value::Null
                        } else {
                            Value::Str(format!("日本λ-{id}"))
                        },
                    ),
                ]
                .into(),
            )
            .unwrap();
    }
    let mut columns = vec!["id".into(), "value".into()];
    if matches!(projection, Projection::Metadata) {
        columns.push("_doc_id".into());
    }
    let expression = if matches!(fault, Fault::Divide) {
        ScalarExpr::Binary {
            op: BinaryOp::Less,
            lhs: Box::new(ScalarExpr::Binary {
                op: BinaryOp::Divide,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(12))),
                rhs: Box::new(ScalarExpr::Binary {
                    op: BinaryOp::Subtract,
                    lhs: Box::new(ScalarExpr::Column("id".into())),
                    rhs: Box::new(ScalarExpr::Literal(Value::Int(4))),
                }),
            }),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
        }
    } else {
        ScalarExpr::Binary {
            op: BinaryOp::Greater,
            lhs: Box::new(ScalarExpr::Column("id".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int((count / 2) as i64))),
        }
    };
    let predicate = crate::ProjectedPredicate::compile(&expression, &columns, &[])
        .unwrap()
        .unwrap();
    let source = ScoredDocumentSource::new(
        "t",
        Arc::new(Table(RwLock::new(Box::new(Documents {
            inner,
            counts: Arc::clone(&counts),
            borrow: !matches!(read, ReadMode::Shared),
            disappear: matches!(read, ReadMode::Lost),
            fault,
        })))),
        ScoredInput::All,
        columns,
        Some("id".into()),
        Some(predicate),
    )
    .with_scan_batch_bytes(budget);
    let key = if matches!(projection, Projection::Metadata) {
        "_doc_id"
    } else {
        "id"
    };
    let key = if matches!(projection, Projection::Computed) {
        ScalarExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(ScalarExpr::Column(key.into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        }
    } else {
        ScalarExpr::Column(key.into())
    };
    let projection = Project::new(
        Box::new(TableScan::new(Box::new(source))),
        vec![
            ("body".into(), ScalarExpr::Column("value".into())),
            ("key".into(), key),
        ],
        Vec::new(),
    );
    let input: Box<dyn PhysicalOperator> = Box::new(ColumnSelection::with_positions(
        Box::new(projection),
        vec![("same".into(), 0), ("key".into(), 1), ("same".into(), 1)],
    ));
    (input, counts)
}

#[test]
fn native_public_materialization_avoids_shared_row_ownership_and_preserves_positions() {
    for budget in [0, 1024, 65536] {
        let (actual, counts) = run(ReadMode::Borrowed, 2048, budget, Projection::Plain);
        let actual = actual.unwrap();
        let (reference, fallback) = run(ReadMode::Shared, 2048, budget, Projection::Plain);
        let reference = reference.unwrap();
        assert_eq!(actual.columns, reference.columns);
        assert_eq!(actual.column_types, reference.column_types);
        assert_eq!(actual.rows, reference.rows);
        assert_eq!(actual.positional_rows, reference.positional_rows);
        assert_eq!(actual.rows.len(), 1024);
        for (offset, values) in actual.positional_rows.as_ref().unwrap().iter().enumerate() {
            let id = offset + 1025;
            let body = if id % 3 == 0 {
                Value::Null
            } else {
                Value::Str(format!("日本λ-{id}"))
            };
            assert_eq!(
                values,
                &[body.clone(), Value::Int(id as i64), Value::Int(id as i64)]
            );
            assert_eq!(actual.rows[offset]["same"], body);
            assert_eq!(actual.rows[offset]["same_1"], Value::Int(id as i64));
        }
        assert_eq!(counts.borrowed.load(Ordering::Relaxed), 2048);
        assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
        assert_eq!(fallback.borrowed.load(Ordering::Relaxed), 0);
        assert!(fallback.shared.load(Ordering::Relaxed) >= 2048);
    }
}

#[test]
fn native_public_materialization_keeps_computed_and_metadata_fallbacks() {
    for projection in [Projection::Computed, Projection::Metadata] {
        let (actual, counts) = run(ReadMode::Borrowed, 8, 1024, projection);
        let (reference, _) = run(ReadMode::Shared, 8, 1024, projection);
        let actual = actual.unwrap();
        let reference = reference.unwrap();
        assert_eq!(actual.rows, reference.rows);
        assert_eq!(actual.positional_rows, reference.positional_rows);
        assert_eq!(counts.borrowed.load(Ordering::Relaxed), 0);
        assert!(counts.shared.load(Ordering::Relaxed) >= 8);
    }
}

#[test]
fn native_public_materialization_preserves_empty_results_and_rejects_lost_capability() {
    let (result, counts) = run(ReadMode::Borrowed, 0, 0, Projection::Plain);
    assert!(result.unwrap().rows.is_empty());
    assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
    let (result, counts) = run(ReadMode::Lost, 8, 1024, Projection::Plain);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stable borrowed projection became unavailable"));
    assert_eq!(counts.shared.load(Ordering::Relaxed), 0);
}
