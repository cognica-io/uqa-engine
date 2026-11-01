//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{ScoredDocumentSource, ScoredInput, METADATA_PREPARATIONS};
use crate::{query::table_read::TableRead, AggregateExecutor, ProjectedRow, RowSource};
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_core::{ScoredEntry, Value};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{DocumentStore, MemoryDocumentStore};

struct Table {
    documents: RwLock<Box<dyn DocumentStore>>,
    columns: Vec<ColumnDef>,
}

impl TableRead for Table {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        self.columns.clone()
    }

    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.documents.read()
    }

    fn maps_integer_keys(&self) -> bool {
        true
    }
}

struct Aggregate {
    borrowed: bool,
    scored: bool,
    metadata: bool,
    rows: usize,
}

impl AggregateExecutor for Aggregate {
    fn consume(&mut self, _: crate::Batch) -> crate::ExecResult<()> {
        panic!("projected aggregation must not materialize input batches")
    }

    fn supports_projected_rows(&self) -> bool {
        true
    }

    fn supports_storage_borrowed_rows(&self) -> bool {
        self.borrowed
    }

    fn consume_projected_row(&mut self, row: &ProjectedRow<'_, '_>) -> crate::ExecResult<()> {
        self.rows += 1;
        let id = self.rows as i64;
        assert_eq!(row.positional_column(0), Some(&Value::Int(id * 2)));
        if self.metadata {
            assert_eq!(row.positional_column(1), Some(&Value::Int(id)));
            assert_eq!(
                row.positional_column(2),
                Some(&Value::Float(if self.scored {
                    id as f64 / 4.0
                } else {
                    0.0
                }))
            );
            assert_eq!(row.positional_column(3), Some(&Value::Int(42)));
            assert_eq!(row.positional_column(4), Some(&Value::Int(id)));
            assert_eq!(row.positional_column(5), Some(&Value::Null));
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::ExecResult<crate::spill::SpillBuffer> {
        unreachable!()
    }
}

#[test]
fn projected_aggregation_prepares_metadata_per_batch_instead_of_per_row() {
    let count = crate::DEFAULT_BATCH_SIZE * 2 + 3;
    let mut documents = MemoryDocumentStore::new();
    for id in 1..=count {
        documents
            .put(
                id as u64,
                [("value".into(), Value::Int(id as i64 * 2))].into(),
            )
            .unwrap();
    }
    let table: Arc<dyn TableRead> = Arc::new(Table {
        documents: RwLock::new(Box::new(documents)),
        columns: Vec::new(),
    });
    for (scored, borrowed) in [(false, true), (true, true), (true, false)] {
        for metadata in [false, true] {
            let input = if scored {
                ScoredInput::entries(
                    (1..=count)
                        .map(|id| ScoredEntry {
                            doc_id: id as u64,
                            score: id as f64 / 4.0,
                        })
                        .collect(),
                    metadata,
                )
            } else {
                ScoredInput::All
            };
            let fields = if metadata {
                vec![
                    "value", "_doc_id", "_score", "tableoid", "_doc_id", "missing",
                ]
            } else {
                vec!["value"]
            };
            let mut source = ScoredDocumentSource::new(
                "t",
                Arc::clone(&table),
                input,
                fields.into_iter().map(String::from).collect(),
                None,
                None,
            )
            .with_table_oid(42);
            let mut aggregate = Aggregate {
                borrowed,
                scored,
                metadata,
                rows: 0,
            };
            let before = METADATA_PREPARATIONS.with(std::cell::Cell::get);
            assert!(source.consume_into_aggregate(&mut aggregate).unwrap());
            assert_eq!(aggregate.rows, count);
            let prepared = METADATA_PREPARATIONS.with(std::cell::Cell::get) - before;
            assert!(
                prepared <= count.div_ceil(crate::DEFAULT_BATCH_SIZE) + 1,
                "metadata preparation scaled with rows: {prepared}, scored={scored}, borrowed={borrowed}, metadata={metadata}"
            );
        }
    }
}

#[derive(Default)]
struct Rows(Vec<Vec<Value>>);

impl AggregateExecutor for Rows {
    fn consume(&mut self, _: crate::Batch) -> crate::ExecResult<()> {
        panic!("projected aggregation must not materialize input batches")
    }

    fn supports_projected_rows(&self) -> bool {
        true
    }

    fn supports_storage_borrowed_rows(&self) -> bool {
        true
    }

    fn consume_projected_row(&mut self, row: &ProjectedRow<'_, '_>) -> crate::ExecResult<()> {
        self.0.push(
            (0..row.row_schema().len())
                .map(|slot| row.positional_column(slot).cloned().unwrap_or(Value::Null))
                .collect(),
        );
        Ok(())
    }

    fn finish(&mut self) -> crate::ExecResult<crate::spill::SpillBuffer> {
        unreachable!()
    }
}

#[test]
fn projected_aggregation_keeps_declared_metadata_columns_and_repeated_slots() {
    let uqa_sql::Statement::CreateTable(declaration) = uqa_sql::compile(
        "CREATE TABLE t (_doc_id BIGINT, _score DOUBLE PRECISION, tableoid BIGINT)",
    )
    .unwrap()
    .remove(0) else {
        panic!("table declaration")
    };
    let mut documents = MemoryDocumentStore::new();
    for (id, value) in [(u64::MAX - 1, 11), (u64::MAX, 23)] {
        documents
            .put(
                id,
                [
                    ("_doc_id".into(), Value::Int(value)),
                    ("_score".into(), Value::Float(value as f64 / 4.0)),
                    ("tableoid".into(), Value::Int(value + 100)),
                ]
                .into(),
            )
            .unwrap();
    }
    let table: Arc<dyn TableRead> = Arc::new(Table {
        documents: RwLock::new(Box::new(documents)),
        columns: declaration.columns,
    });
    let mut source = ScoredDocumentSource::new(
        "t",
        table,
        ScoredInput::All,
        ["_doc_id", "_score", "tableoid", "_doc_id"]
            .map(String::from)
            .to_vec(),
        None,
        None,
    )
    .with_table_oid(42);
    let mut rows = Rows::default();
    assert!(source.consume_into_aggregate(&mut rows).unwrap());
    assert_eq!(
        rows.0,
        [11, 23].map(|value| vec![
            Value::Int(value),
            Value::Float(value as f64 / 4.0),
            Value::Int(value + 100),
            Value::Int(value),
        ])
    );
}

#[test]
fn projected_aggregation_reads_row_metadata_only_after_its_predicate() {
    let mut documents = MemoryDocumentStore::new();
    for (id, value) in [(7, 1), (u64::MAX, 99)] {
        documents
            .put(id, [("value".into(), Value::Int(value))].into())
            .unwrap();
    }
    let table: Arc<dyn TableRead> = Arc::new(Table {
        documents: RwLock::new(Box::new(documents)),
        columns: Vec::new(),
    });
    for filtered in [true, false] {
        let fields = ["value", "_doc_id", "tableoid"].map(String::from).to_vec();
        let predicate = filtered.then(|| {
            crate::ProjectedPredicate::compile(
                &crate::ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Less,
                    lhs: Box::new(crate::ScalarExpr::Column("value".into())),
                    rhs: Box::new(crate::ScalarExpr::Literal(Value::Int(10))),
                },
                &["value".into()],
                &[],
            )
            .unwrap()
            .unwrap()
        });
        let mut source = ScoredDocumentSource::new(
            "t",
            Arc::clone(&table),
            ScoredInput::All,
            fields,
            None,
            predicate,
        );
        let mut rows = Rows::default();
        let result = source.consume_into_aggregate(&mut rows);
        if filtered {
            assert!(result.unwrap());
        } else {
            assert!(matches!(result,
                Err(crate::ExecError::SQL(uqa_sql::SQLError::TypeMismatch(message)))
                if message == "document id 18446744073709551615 exceeds the SQL BIGINT range"
            ));
        }
        assert_eq!(rows.0, [vec![Value::Int(1), Value::Int(7), Value::Null]]);
    }
}
