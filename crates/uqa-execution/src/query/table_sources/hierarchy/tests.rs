//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::{table_read::TableRead, table_sources::retrieval::RetrievalAccess};
use crate::RowSource;
use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use uqa_core::Value;
use uqa_sql::ast::ColumnDef;
use uqa_storage::{DocumentStore, MemoryDocumentStore, StoredDocument};

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

#[derive(Default)]
struct Retrieval {
    committed: Mutex<Vec<bool>>,
    fail: bool,
    capture: bool,
}

impl RetrievalAccess for Retrieval {
    fn direct_vector_retrieval(
        &self,
        _: &ScalarExpr,
        _: &[SQLParam],
    ) -> Result<Option<DirectVectorRetrieval>, SQLError> {
        Ok(None)
    }
    fn knn_entries(
        &self,
        _: &str,
        _: &str,
        _: &[f32],
        _: usize,
        _: bool,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        unreachable!("this fixture exercises ordinary retrieval")
    }
    fn retrieval_entries(
        &self,
        _: &str,
        _: &ScalarExpr,
        _: &[SQLParam],
        committed: bool,
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        self.committed.lock().push(committed);
        if self.fail {
            return Err(SQLError::Internal("selected retrieval failure".into()));
        }
        Ok(Some(vec![ScoredEntry {
            doc_id: 1,
            score: 0.75,
        }]))
    }
    fn retrieval_input(
        &self,
        table: &str,
        predicate: &ScalarExpr,
        params: &[SQLParam],
        committed: bool,
    ) -> Result<Option<crate::query::scored_input::ProducedScoredEntries>, SQLError> {
        let entries = self.retrieval_entries(table, predicate, params, committed)?;
        entries
            .map(|entries| {
                if !self.capture {
                    return Ok(entries.into());
                }
                let mut capture = crate::query::scored_input::ScoredEntryCapture::new(
                    0,
                    uqa_core::CancellationToken::new(),
                )?;
                for entry in entries {
                    capture.push(entry)?;
                }
                capture
                    .finish()
                    .map(crate::query::scored_input::ProducedScoredEntries::Captured)
            })
            .transpose()
    }
}

fn source(retrieval: &Retrieval, recheck: bool, hierarchy: bool) -> Box<dyn PhysicalOperator + '_> {
    let mut documents = MemoryDocumentStore::new();
    documents
        .put(1, [("id".into(), Value::Int(1))].into())
        .unwrap();
    let source = ScoredDocumentSource::new(
        "docs",
        Arc::new(Table(RwLock::new(Box::new(documents)))),
        ScoredInput::entries(Vec::new(), true),
        vec!["id".into()],
        None,
        None,
    );
    let schema = source.physical_schema().unwrap().clone();
    let physical = PhysicalRetrieval {
        table_name: "docs".into(),
        entries: Vec::new(),
        lock_origin: None,
        recheck_pins: recheck.then(|| {
            Arc::new(vec![RecheckDoc {
                doc_id: 1,
                document: Some(Arc::new(StoredDocument::new(
                    [("id".into(), Value::Int(2))].into(),
                ))),
            }])
        }),
    };
    if hierarchy {
        Box::new(source::deferred(
            retrieval,
            vec![physical],
            None,
            ScalarExpr::Literal(Value::Bool(true)),
            &[],
            vec![source],
            schema,
        ))
    } else {
        crate::query::scored_input::defer_entries(
            source,
            Some(Box::new(move || {
                retrieval
                    .retrieval_entries(
                        "docs",
                        &ScalarExpr::Literal(Value::Bool(true)),
                        &[],
                        recheck,
                    )
                    .map(Option::unwrap)
                    .map(Into::into)
            })),
            None,
            physical.recheck_pins,
        )
        .unwrap()
    }
}

#[test]
fn an_unconsumed_retrieval_never_executes_even_when_opened_and_closed() {
    for hierarchy in [false, true] {
        let retrieval = Retrieval::default();
        let mut source = source(&retrieval, false, hierarchy);
        assert!(source.schema().iter().any(|column| column == "id"));
        source.open().unwrap();
        source.close().unwrap();
        assert!(retrieval.committed.lock().is_empty());
    }
}

#[test]
fn demanded_retrieval_preserves_schema_scores_and_current_recheck_pins_without_replay() {
    for (recheck, hierarchy) in [(false, false), (true, false), (false, true), (true, true)] {
        for fail in [false, true] {
            let retrieval = Retrieval {
                fail,
                ..Retrieval::default()
            };
            let mut source = source(&retrieval, recheck, hierarchy);
            let schema = source.row_schema().clone();
            source.open().unwrap();
            assert!(retrieval.committed.lock().is_empty());
            if fail {
                assert!(source
                    .next()
                    .unwrap_err()
                    .to_string()
                    .contains("selected retrieval failure"));
                assert!(source.next().is_err());
            } else {
                let batch = source.next().unwrap().unwrap();
                assert_eq!(batch.schema, schema);
                let rows = batch.into_result_rows();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0]["id"], Value::Int(if recheck { 2 } else { 1 }));
                assert_eq!(rows[0]["_score"], Value::Float(0.75));
                assert!(source.next().unwrap().is_none());
            }
            source.close().unwrap();
            assert_eq!(*retrieval.committed.lock(), vec![recheck]);
        }
    }
}

#[test]
fn captured_hierarchy_input_preserves_deferred_errors_and_committed_recheck_pins() {
    for recheck in [false, true] {
        for fail in [false, true] {
            let retrieval = Retrieval {
                capture: true,
                fail,
                ..Retrieval::default()
            };
            let mut unconsumed = source(&retrieval, recheck, true);
            unconsumed.open().unwrap();
            unconsumed.close().unwrap();
            assert!(retrieval.committed.lock().is_empty());
            let mut rows = source(&retrieval, recheck, true);
            let schema = rows.row_schema().clone();
            rows.open().unwrap();
            assert!(retrieval.committed.lock().is_empty());
            if fail {
                let error = rows.next().unwrap_err();
                assert!(error.to_string().contains("selected retrieval failure"));
                assert!(rows.next().is_err());
            } else {
                let batch = rows.next().unwrap().unwrap();
                assert_eq!(batch.schema, schema);
                let result = batch.into_result_rows();
                assert_eq!(result.len(), 1);
                assert_eq!(result[0]["id"], Value::Int(if recheck { 2 } else { 1 }));
                assert_eq!(result[0]["_score"], Value::Float(0.75));
                assert!(rows.next().unwrap().is_none());
            }
            rows.close().unwrap();
            assert_eq!(*retrieval.committed.lock(), vec![recheck]);
        }
    }
}
