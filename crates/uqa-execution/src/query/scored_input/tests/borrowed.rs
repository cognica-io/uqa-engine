//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::scored_input::{ScoredDocumentSource, ScoredInput};
use crate::query::table_read::TableRead;
use crate::{RowSource, ScalarExpr};
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_core::{DocId, Value};
use uqa_sql::ast::{BinaryOp, ColumnDef};
use uqa_storage::{DocumentStore, MemoryDocumentStore, StorageBackendResult, StoredDocument};

struct CursorDocuments(MemoryDocumentStore, bool);

impl DocumentStore for CursorDocuments {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("scan must not point-read rows")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("scan must not split identity and value reads")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.0.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        unreachable!()
    }
    fn for_each_fields_multi_ref_with_presence(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        assert!(
            self.1,
            "pure point consumers must use the borrowed projection"
        );
        self.0
            .for_each_fields_multi_ref_with_presence(ids, fields, visitor)
    }
    fn for_each_fields_multi_borrowed(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        assert!(!self.1, "an unrestricted aggregate must not borrow storage");
        let mut stopped = false;
        let result =
            self.0
                .for_each_fields_multi_borrowed(ids, fields, &mut |id, present, values| {
                    let more = visitor(id, present, values);
                    stopped = !more;
                    more
                })?;
        if stopped {
            return Err(uqa_storage::StorageBackendError::Other(
                "post-visitor failure".into(),
            ));
        }
        Ok(result)
    }
    fn for_each_next_fields_borrowed(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        assert!(
            !self.1,
            "stable shared rows must not be copied through the borrowed cursor"
        );
        let mut stopped = false;
        let result =
            self.0
                .for_each_next_fields(after, limit.min(1), fields, &mut |id, values| {
                    let more = visitor(id, values);
                    stopped = !more;
                    more
                })?;
        if stopped {
            return Err(uqa_storage::StorageBackendError::Other(
                "post-visitor failure".into(),
            ));
        }
        Ok(result)
    }
    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, uqa_storage::document_store::SharedDocumentRow)>>>
    {
        if self.1 {
            self.0.next_shared_fields(after, limit.min(1), fields)
        } else {
            Ok(None)
        }
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

fn table() -> Arc<dyn TableRead> {
    table_with_shared(false)
}

fn table_with_shared(shared: bool) -> Arc<dyn TableRead> {
    let mut documents = MemoryDocumentStore::new();
    for id in [1, 3, 5, 7, 9] {
        documents
            .put(id, [("key".into(), Value::Int(id as i64))].into())
            .unwrap();
    }
    Arc::new(Table(RwLock::new(Box::new(CursorDocuments(
        documents, shared,
    )))))
}

fn predicate() -> crate::ProjectedPredicate {
    crate::ProjectedPredicate::compile_with_schema(
        &ScalarExpr::Binary {
            op: BinaryOp::Greater,
            lhs: Box::new(ScalarExpr::Column("key".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(5))),
        },
        &crate::RowSchema::new(vec!["key".into(), "missing".into()]),
        &[],
    )
    .unwrap()
    .unwrap()
}

#[test]
fn borrowed_scored_scan_advances_rejected_pages_and_preserves_metadata() {
    let mut source = ScoredDocumentSource::new(
        "t",
        table(),
        ScoredInput::All,
        vec!["key".into(), "missing".into(), "_doc_id".into()],
        None,
        Some(predicate()),
    );
    assert!(source.next_physical_batch(0).unwrap().is_empty());
    let rows = source.next_physical_batch(2).unwrap();
    assert_eq!(rows.len(), 2);
    for (row, id) in rows.iter().zip([7, 9]) {
        assert_eq!(row.value(0), Some(&Value::Int(id)));
        assert_eq!(row.value(1), Some(&Value::Null));
        assert_eq!(row.value(2), Some(&Value::Int(id)));
    }
    assert!(source.next_physical_batch(2).unwrap().is_empty());
}

#[test]
fn borrowed_local_scan_preserves_lock_identity_and_cancellation() {
    use crate::query::local_table::{LocalTableRowSource, LocalTableScanConfig};
    for shared in [false, true] {
        let cancellation = uqa_core::CancellationToken::new();
        let columns = vec!["key".into(), "missing".into()];
        let mut source = LocalTableRowSource::new(LocalTableScanConfig {
            cancellation: cancellation.clone(),
            serializable: None,
            table_name: "t".into(),
            table: table_with_shared(shared),
            column_definitions: Arc::new(Vec::new()),
            columns: columns.clone(),
            schema: columns.clone(),
            physical_schema: crate::RowSchema::new(columns),
            metadata: uqa_sql::plan::source_projection::RelationMetadataProjection::default(),
            table_oid: None,
            predicate: Some(predicate()),
            estimated_cardinality: 5,
            lock_origin: Some(("t".into(), "public.t".into())),
            recheck_pins: None,
            command_changes: None,
        });
        assert!(source.next_physical_batch(0).unwrap().is_empty());
        let rows = source.next_physical_batch(2).unwrap();
        assert_eq!(rows.len(), 2);
        for (row, id) in rows.iter().zip([7, 9]) {
            assert_eq!(row.value(0), Some(&Value::Int(id)));
            assert_eq!(row.value(1), Some(&Value::Null));
            assert_eq!(row.lock_origins()[0].doc_id, id as u64);
        }
        assert!(source.next_physical_batch(1).unwrap().is_empty());
        cancellation.cancel();
        assert!(source.next_physical_batch(1).is_err());
    }
}

#[test]
fn borrowed_scan_preserves_the_predicate_error_before_provider_cleanup_failure() {
    let predicate = || {
        crate::ProjectedPredicate::compile_with_schema(
            &ScalarExpr::Binary {
                op: BinaryOp::Greater,
                lhs: Box::new(ScalarExpr::Binary {
                    op: BinaryOp::Divide,
                    lhs: Box::new(ScalarExpr::Column("key".into())),
                    rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
                }),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
            },
            &crate::RowSchema::new(vec!["key".into()]),
            &[],
        )
        .unwrap()
        .expect("pure arithmetic predicate")
    };
    let mut source = ScoredDocumentSource::new(
        "t",
        table(),
        ScoredInput::All,
        vec!["key".into()],
        None,
        Some(predicate()),
    );
    let error = source.next_physical_batch(2).unwrap_err();
    assert!(error.to_string().contains("division by zero"), "{error}");
    let source = ScoredDocumentSource::new(
        "t",
        table(),
        ScoredInput::All,
        vec!["key".into()],
        None,
        Some(predicate()),
    );
    let error = source
        .materialize_physical_entries(&[
            uqa_core::ScoredEntry {
                doc_id: 1,
                score: 0.0,
            },
            uqa_core::ScoredEntry {
                doc_id: 3,
                score: 0.0,
            },
        ])
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"), "{error}");
}

#[test]
fn borrowed_scored_points_keep_duplicate_ranking_positions() {
    let mut source = ScoredDocumentSource::new(
        "t",
        table(),
        ScoredInput::entries(
            vec![
                uqa_core::ScoredEntry {
                    doc_id: 7,
                    score: 0.9,
                },
                uqa_core::ScoredEntry {
                    doc_id: 3,
                    score: 0.8,
                },
                uqa_core::ScoredEntry {
                    doc_id: 7,
                    score: 0.7,
                },
            ],
            true,
        ),
        vec!["key".into(), "_score".into(), "_doc_id".into()],
        None,
        None,
    );
    let rows = source.next_physical_batch(3).unwrap();
    assert_eq!(rows.len(), 3);
    for (row, (id, score)) in rows.iter().zip([(7, 0.9), (3, 0.8), (7, 0.7)]) {
        assert_eq!(row.value(0), Some(&Value::Int(id)));
        assert_eq!(row.value(1), Some(&Value::Float(score)));
        assert_eq!(row.value(2), Some(&Value::Int(id)));
    }
}

mod point_aggregate;
