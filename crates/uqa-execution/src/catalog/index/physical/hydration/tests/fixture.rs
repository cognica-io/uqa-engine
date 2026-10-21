//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::{Mutex, RwLock};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use uqa_sql::{ast::*, catalog::index::EnforcedKey, RowSchema, SQLParam};
use uqa_storage::{
    document_store::{Document, MemoryDocumentStore, StoredDocument},
    DocumentStore,
};

#[derive(Default)]
pub(super) struct Reads {
    pub ids_complete: AtomicBool,
    pub pages: AtomicUsize,
    pub documents: Mutex<Vec<DocId>>,
    pub evaluations: AtomicUsize,
}

pub(super) struct Documents {
    pub store: RwLock<Box<dyn DocumentStore>>,
    pub reads: Arc<Reads>,
    pub count: usize,
}

impl Documents {
    pub fn new(count: usize, width: usize, fail: Option<DocId>, malformed: bool) -> Self {
        let mut inner = MemoryDocumentStore::default();
        for id in 1..=count as u64 {
            inner
                .put_stored(
                    id,
                    StoredDocument::new(Document::from([(
                        "key".into(),
                        if width == 0 {
                            Value::Int(id as i64)
                        } else {
                            Value::Str(format!("{id:04}{}", "x".repeat(width)))
                        },
                    )])),
                )
                .unwrap();
        }
        let reads = Arc::new(Reads::default());
        Self {
            store: RwLock::new(Box::new(Store {
                inner,
                reads: reads.clone(),
                fail,
                malformed,
            })),
            reads,
            count,
        }
    }
}

impl IndexDocuments for Documents {
    fn read(&self) -> Box<dyn std::ops::Deref<Target = Box<dyn DocumentStore>> + '_> {
        Box::new(self.store.read())
    }
}

struct Store {
    inner: MemoryDocumentStore,
    reads: Arc<Reads>,
    fail: Option<DocId>,
    malformed: bool,
}

impl DocumentStore for Store {
    fn put_stored(&mut self, id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.inner.put_stored(id, document)
    }
    fn get_stored(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        assert!(
            self.reads.ids_complete.load(Ordering::Relaxed),
            "payload read before complete identity capture"
        );
        self.reads.documents.lock().push(id);
        if self.fail == Some(id) {
            return Err(StorageBackendError::backend(
                "later read",
                SQLError::Routine {
                    sqlstate: "53200".into(),
                    message: "later document read".into(),
                },
            ));
        }
        self.inner.get_stored(id)
    }
    fn delete(&mut self, id: DocId) -> StorageBackendResult<()> {
        self.inner.delete(id)
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.inner.clear()
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("unbounded identity read")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        self.inner.len()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.inner.snapshot()
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<DocId>> {
        self.reads.pages.fetch_add(1, Ordering::Relaxed);
        let ids = self.inner.next_doc_ids_controlled(after, limit, control)?;
        if ids.is_empty() {
            self.reads.ids_complete.store(true, Ordering::Relaxed);
        }
        Ok(ids)
    }
    fn get_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut result = BTreeMap::new();
        for &id in ids {
            if let Some(document) = self.get(id)? {
                result.insert(
                    id,
                    if self.malformed && id == 1 {
                        Vec::new()
                    } else {
                        fields
                            .iter()
                            .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
                            .collect()
                    },
                );
            }
        }
        Ok(result)
    }
}

impl uqa_sql::semantics::conflict::ConflictCatalog for Documents {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(Some(Vec::new()))
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<EnforcedKey>, String> {
        unreachable!()
    }
    fn try_declared_table_constraints(&self, _: &str) -> Result<TableConstraintSet, String> {
        unreachable!()
    }
}

impl uqa_sql::semantics::partition::PartitionExpressions for Documents {
    fn expression_text(&self, expression: &Expr) -> Result<String, SQLError> {
        uqa_sql::catalog::expression_text::schema_expr_text(expression)
    }
    fn evaluate_bound(&self, _: &Expr, _: &[SQLParam]) -> Result<Value, SQLError> {
        unreachable!()
    }
    fn evaluate_row(
        &self,
        expression: &Expr,
        row: &dyn uqa_sql::expr::RowLookup,
        _: &RowSchema,
        _: &[SQLParam],
    ) -> Result<Value, SQLError> {
        assert!(
            self.store.try_write().is_some(),
            "document guard crossed expression callback"
        );
        let ordinal = self.reads.evaluations.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            self.reads.documents.lock().len(),
            ((ordinal / 256 + 1) * 256).min(self.count),
            "a complete original 256-row batch must precede its first callback"
        );
        match expression {
            Expr::Column(name) => Ok(row.column(name).unwrap().clone()),
            _ => unreachable!(),
        }
    }
}
