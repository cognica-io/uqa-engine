//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::{
    collection::collect_query_operator, relational::values::test_support::Services,
    runtime::QueryRuntimeView, statement::bound_consumer::bind_output_mode,
};
use crate::{Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, RowSchema};
use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;

struct Source {
    schema: RowSchema,
    values: Arc<Vec<Value>>,
    projection: Arc<[usize]>,
    remaining: usize,
    observed_owners: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    pulls: Arc<AtomicUsize>,
    fail_after: Option<usize>,
    emitted: Arc<AtomicUsize>,
}

impl Source {
    fn new(remaining: usize) -> Self {
        Self {
            schema: RowSchema::new(vec!["value".into()]),
            values: Arc::new(vec![Value::Str("日本λ".into())]),
            projection: Arc::from([0]),
            remaining,
            observed_owners: Arc::default(),
            closes: Arc::default(),
            pulls: Arc::default(),
            fail_after: None,
            emitted: Arc::default(),
        }
    }

    fn pull(&mut self, maximum: usize) -> ExecResult<Option<Batch>> {
        self.pulls.fetch_add(1, Ordering::Relaxed);
        self.observed_owners
            .fetch_max(Arc::strong_count(&self.values), Ordering::Relaxed);
        if self
            .fail_after
            .is_some_and(|limit| self.emitted.load(Ordering::Relaxed) >= limit)
        {
            return Err(ExecError::SQL(uqa_sql::SQLError::Routine {
                sqlstate: "22012".into(),
                message: "late input failure".into(),
            }));
        }
        let available = self.fail_after.map_or(self.remaining, |limit| {
            limit.saturating_sub(self.emitted.load(Ordering::Relaxed))
        });
        let count = self.remaining.min(maximum).min(available);
        self.remaining -= count;
        self.emitted.fetch_add(count, Ordering::Relaxed);
        Ok((count != 0).then(|| {
            let rows = (0..count)
                .map(|_| {
                    PhysicalRow::from_shared_values(
                        Arc::clone(&self.values),
                        Arc::clone(&self.projection),
                    )
                })
                .collect();
            Batch::from_physical_rows(self.schema.clone(), rows)
        }))
    }
}

impl PhysicalOperator for Source {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn supports_batched_consumption(&self) -> bool {
        true
    }

    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(128)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(rows.get().min(128))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn public_result_collection_releases_physical_batches_before_reading_the_next() {
    const COUNT: usize = 16384;
    let source = Source::new(COUNT);
    let observed_owners = Arc::clone(&source.observed_owners);
    let closes = Arc::clone(&source.closes);
    let pulls = Arc::clone(&source.pulls);
    let projection = crate::query::ordering::append_row_at_time_projection(
        Box::new(source),
        vec![(
            crate::ProjectionTarget::Column("selected".into()),
            crate::ScalarExpr::Column("value".into()),
        )],
        crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
    );
    let selection =
        crate::ColumnSelection::with_mapping(projection, vec![("value".into(), "selected".into())]);
    let result = super::collect::<()>(|mode| {
        collect_input(Box::new(selection), bind_output_mode(None, mode, None)?)
    })
    .unwrap();
    assert_eq!(result.columns, ["value"]);
    assert_eq!(result.rows.len(), COUNT);
    assert!(result
        .rows
        .iter()
        .all(|row| matches!(row.get("value"), Some(Value::Str(value)) if value == "日本λ")));
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert_eq!(
        pulls.load(Ordering::Relaxed),
        COUNT.div_ceil(128) + 1,
        "complete public output must keep native batches instead of one pull per row"
    );
    assert_eq!(
        observed_owners.load(Ordering::Relaxed),
        1,
        "completed public rows kept earlier physical input batches alive"
    );
}

fn collect_source(source: Source) -> Result<uqa_sql::SQLResult, uqa_sql::SQLError> {
    super::collect::<()>(|mode| {
        collect_input(Box::new(source), bind_output_mode(None, mode, None)?)
    })
}

struct Counted {
    input: Box<dyn PhysicalOperator>,
    calls: Arc<AtomicUsize>,
}

impl PhysicalOperator for Counted {
    fn row_schema(&self) -> &RowSchema {
        self.input.row_schema()
    }
    fn supports_batched_consumption(&self) -> bool {
        self.input.supports_batched_consumption()
    }
    fn open(&mut self) -> ExecResult<()> {
        self.input.open()
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.input.next()
    }
    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.input.next_with_demand(rows)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.input.close()
    }
}

fn stored_scan(count: usize) -> crate::TableScan {
    use crate::query::{
        scored_input::{ScoredDocumentSource, ScoredInput},
        table_read::TableRead,
    };
    use uqa_storage::{DocumentStore, MemoryDocumentStore};

    struct Table(RwLock<Box<dyn DocumentStore>>);
    impl TableRead for Table {
        fn column_definitions(&self) -> Vec<uqa_sql::ast::ColumnDef> {
            Vec::new()
        }
        fn read_documents(&self) -> parking_lot::RwLockReadGuard<'_, Box<dyn DocumentStore>> {
            self.0.read()
        }
        fn maps_integer_keys(&self) -> bool {
            true
        }
    }
    let mut documents = MemoryDocumentStore::new();
    for id in 1..=count {
        documents
            .put(
                id as u64,
                [
                    ("id".into(), Value::Int(id as i64)),
                    ("value".into(), Value::Str(format!("日本λ-{id}"))),
                ]
                .into(),
            )
            .unwrap();
    }
    let columns = vec!["id".into(), "value".into()];
    let predicate = crate::ProjectedPredicate::compile(
        &crate::ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Greater,
            lhs: Box::new(crate::ScalarExpr::Column("id".into())),
            rhs: Box::new(crate::ScalarExpr::Literal(Value::Int((count / 2) as i64))),
        },
        &columns,
        &[],
    )
    .unwrap()
    .unwrap();
    let source = ScoredDocumentSource::new(
        "t",
        Arc::new(Table(RwLock::new(Box::new(documents)))),
        ScoredInput::All,
        columns,
        Some("id".into()),
        Some(predicate),
    );
    crate::TableScan::new(Box::new(source))
}

#[test]
fn complete_result_collection_preserves_stored_scan_and_sort_batches() {
    const COUNT: usize = 4096;
    for sorted in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let scan: Box<dyn PhysicalOperator> = Box::new(stored_scan(COUNT * 2));
        let input: Box<dyn PhysicalOperator> = if sorted {
            Box::new(crate::ExternalSort::new(
                scan,
                vec![crate::SortKey {
                    expr: crate::ScalarExpr::Column("id".into()),
                    descending: true,
                    nulls_first: None,
                }],
                crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
                None,
                4 * 1024 * 1024,
            ))
        } else {
            scan
        };
        let projection = crate::query::ordering::append_row_at_time_projection(
            input,
            vec![(
                crate::ProjectionTarget::Column("selected".into()),
                crate::ScalarExpr::Column("value".into()),
            )],
            crate::relational::DefaultExpressionEvaluator::shared(Vec::new()),
        );
        let selection = crate::ColumnSelection::with_mapping(
            projection,
            vec![("value".into(), "selected".into())],
        );
        let counted = Counted {
            input: Box::new(selection),
            calls: Arc::clone(&calls),
        };
        let result = super::collect::<()>(|mode| {
            collect_input(Box::new(counted), bind_output_mode(None, mode, None)?)
        })
        .unwrap();
        assert_eq!(result.rows.len(), COUNT);
        for (index, row) in result.rows.iter().enumerate() {
            let id = if sorted {
                COUNT * 2 - index
            } else {
                COUNT + index + 1
            };
            assert_eq!(row["value"], Value::Str(format!("日本λ-{id}")));
        }
        assert!(
            calls.load(Ordering::Relaxed) <= COUNT.div_ceil(128) + 2,
            "sorted={sorted}: stored scan or projection split batches into {} pulls",
            calls.load(Ordering::Relaxed)
        );
    }
}

fn collect_input(
    source: Box<dyn PhysicalOperator>,
    mode: crate::query::consumer::QueryOutputMode<'_>,
) -> Result<crate::query::output::QueryOutput, uqa_sql::SQLError> {
    let runtime = QueryRuntimeView {
        function_context: &crate::functions::SQLFunctionContext::empty(),
        diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
        cancellation: &uqa_core::CancellationToken::new(),
        settings: &Services,
        scalar_functions: &RwLock::default(),
        table_functions: &RwLock::default(),
        aggregate_functions: &RwLock::default(),
        notices: &crate::query::NoticeQueue::default(),
    };
    collect_query_operator(runtime, vec!["value".into()], source, mode)
}

#[test]
fn complete_result_collection_preserves_interleaved_row_effects() {
    use uqa_sql::{expr::RowLookup, ScalarExpr};

    struct Effect {
        calls: Arc<AtomicUsize>,
        output: bool,
    }
    impl crate::ExpressionEvaluator for Effect {
        fn evaluate(&self, _: &ScalarExpr, _: &dyn RowLookup) -> ExecResult<Value> {
            let value = i64::try_from(self.calls.fetch_add(1, Ordering::Relaxed) + 1).unwrap();
            Ok(if self.output {
                Value::Int(value)
            } else {
                Value::Bool(true)
            })
        }
    }
    struct Unclassified(Box<dyn PhysicalOperator>);
    impl PhysicalOperator for Unclassified {
        fn row_schema(&self) -> &RowSchema {
            self.0.row_schema()
        }
        fn open(&mut self) -> ExecResult<()> {
            self.0.open()
        }
        fn next(&mut self) -> ExecResult<Option<Batch>> {
            self.0.next()
        }
        fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
            self.0.next_with_demand(rows)
        }
        fn close(&mut self) -> ExecResult<()> {
            self.0.close()
        }
    }

    for opaque in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let source = crate::TableScan::from_physical_rows(
            RowSchema::new(vec!["id".into()]),
            [1, 2]
                .into_iter()
                .map(|id| PhysicalRow::from_values(vec![Value::Int(id)]))
                .collect(),
        );
        let filter = crate::Filter::with_evaluator(
            Box::new(source),
            ScalarExpr::Literal(Value::Bool(true)),
            Arc::new(Effect {
                calls: Arc::clone(&calls),
                output: false,
            }),
        );
        let projection = crate::Project::with_evaluator(
            Box::new(filter),
            vec![("value".into(), ScalarExpr::Literal(Value::Int(0)))],
            Arc::new(Effect {
                calls: Arc::clone(&calls),
                output: true,
            }),
        );
        let producer: Box<dyn PhysicalOperator> = if opaque {
            Box::new(Unclassified(Box::new(projection)))
        } else {
            Box::new(projection)
        };
        let result = super::collect::<()>(|mode| {
            collect_input(producer, bind_output_mode(None, mode, None)?)
        })
        .unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["value"].clone())
                .collect::<Vec<_>>(),
            vec![Value::Int(2), Value::Int(4)]
        );
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }
}

#[test]
fn row_sensitive_consumers_keep_singleton_production_and_first_error() {
    use crate::query::consumer::{QueryConsumerControl, QueryOutputMode, QueryRowConsumer};
    use crate::OwnedPhysicalRow;
    use std::{cell::Cell, rc::Rc};
    use uqa_sql::SQLError;

    struct Consumer {
        remaining: Cell<usize>,
        fail: bool,
    }
    impl QueryRowConsumer for Consumer {
        fn begin(&self, _: &[String], _: &RowSchema) -> Result<(), SQLError> {
            Ok(())
        }

        fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
            assert_eq!(row.get("value"), Some(&Value::Str("日本λ".into())));
            let remaining = self.remaining.get() - 1;
            self.remaining.set(remaining);
            if remaining != 0 {
                return Ok(QueryConsumerControl::Continue);
            }
            if self.fail {
                return Err(SQLError::Routine {
                    sqlstate: "57014".into(),
                    message: "cancelled by consumer".into(),
                });
            }
            Ok(QueryConsumerControl::Stop)
        }
    }

    for fail in [false, true] {
        let mut source = Source::new(128);
        source.fail_after = Some(3);
        let produced = Arc::clone(&source.emitted);
        let pulls = Arc::clone(&source.pulls);
        let closes = Arc::clone(&source.closes);
        let values = Arc::downgrade(&source.values);
        let consumer = Rc::new(Consumer {
            remaining: Cell::new(3),
            fail,
        });
        match collect_input(Box::new(source), QueryOutputMode::RowConsumer(consumer)) {
            Ok(_) => assert!(!fail),
            Err(error) => {
                assert!(fail);
                assert_eq!(error.sqlstate(), Some("57014"));
            }
        }
        assert_eq!(produced.load(Ordering::Relaxed), 3);
        assert_eq!(pulls.load(Ordering::Relaxed), 3);
        assert_eq!(closes.load(Ordering::Relaxed), 1);
        assert!(values.upgrade().is_none());
    }
}

#[test]
fn public_result_collection_preserves_empty_and_late_error_cleanup() {
    let empty = Source::new(0);
    let closes = Arc::clone(&empty.closes);
    let result = collect_source(empty).unwrap();
    assert_eq!(result.columns, ["value"]);
    assert!(result.rows.is_empty());
    assert_eq!(closes.load(Ordering::Relaxed), 1);

    let mut failing = Source::new(256);
    failing.fail_after = Some(128);
    let closes = Arc::clone(&failing.closes);
    let values = Arc::downgrade(&failing.values);
    let error = collect_source(failing).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert!(values.upgrade().is_none());
}

#[test]
fn public_result_collection_preserves_duplicate_slots_and_live_layouts() {
    use crate::query::{
        consumer::QueryConsumerControl,
        output::{QueryOutput, QueryRows},
    };
    use crate::OwnedPhysicalRow;
    use uqa_sql::ColumnType;

    let input = RowSchema::with_column_metadata(
        None,
        [
            ("first".into(), Some(ColumnType::Text), Some(950)),
            ("last".into(), Some(ColumnType::Text), Some(951)),
        ],
    );
    let first = RowSchema::select(
        &input,
        &[
            ("value".into(), "first".into()),
            ("value".into(), "last".into()),
            ("missing".into(), "absent".into()),
        ],
    );
    let second = RowSchema::select(
        &input,
        &[
            ("value".into(), "last".into()),
            ("value".into(), "first".into()),
            ("missing".into(), "absent".into()),
        ],
    );
    for delivery in 0..3 {
        let result = super::collect::<()>(|mode| {
            let crate::query::consumer::QueryOutputMode::RowConsumer(consumer) =
                bind_output_mode(None, mode, None)?
            else {
                panic!("public output must release each consumed batch");
            };
            assert!(!consumer.streams_set_inputs());
            consumer.begin(first.columns(), &first)?;
            for (index, schema) in [&first, &second, &first].into_iter().enumerate() {
                let row = PhysicalRow::from_values(vec![
                    Value::Str("left".into()),
                    Value::Str("right".into()),
                ]);
                let control = if delivery == 1 || (delivery == 2 && index % 2 == 0) {
                    consumer.consume_batch(Batch::from_physical_rows(schema.clone(), vec![row]))?
                } else {
                    consumer.consume(OwnedPhysicalRow::new(schema.clone(), row))?
                };
                assert!(matches!(control, QueryConsumerControl::Continue));
            }
            Ok(QueryOutput {
                columns: first.columns().to_vec(),
                column_types: first.column_types().to_vec(),
                internal_schema: first.clone(),
                rows: QueryRows::Rows(Vec::new()),
            })
        })
        .unwrap();
        assert_eq!(
            result.positional_rows.as_ref().unwrap(),
            &[
                vec![
                    Value::Str("left".into()),
                    Value::Str("right".into()),
                    Value::Null
                ],
                vec![
                    Value::Str("right".into()),
                    Value::Str("left".into()),
                    Value::Null
                ],
                vec![
                    Value::Str("left".into()),
                    Value::Str("right".into()),
                    Value::Null
                ],
            ]
        );
        for (row, (first, last)) in
            result
                .rows
                .iter()
                .zip([("left", "right"), ("right", "left"), ("left", "right")])
        {
            assert_eq!(row["value"], Value::Str(first.into()));
            assert_eq!(row["value_1"], Value::Str(last.into()));
            assert_eq!(row["missing"], Value::Null);
        }
        assert_eq!(result.row_schema().column_collation(0), Some(950));
        assert_eq!(result.row_schema().column_collation(1), Some(951));
    }
}

#[test]
fn default_batch_consumption_preserves_row_control_and_error_order() {
    use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
    use crate::OwnedPhysicalRow;
    use std::cell::Cell;
    use uqa_sql::SQLError;

    struct Consumer {
        count: Cell<usize>,
        control: Option<QueryConsumerControl>,
    }
    impl QueryRowConsumer for Consumer {
        fn begin(&self, _: &[String], _: &RowSchema) -> Result<(), SQLError> {
            Ok(())
        }
        fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
            let next = self.count.get() + 1;
            assert_eq!(
                row.get("value"),
                Some(&Value::Int(i64::try_from(next).unwrap()))
            );
            self.count.set(next);
            if next < 2 {
                return Ok(QueryConsumerControl::Continue);
            }
            self.control.ok_or_else(|| SQLError::Routine {
                sqlstate: "57014".into(),
                message: "cancelled by consumer".into(),
            })
        }
    }
    for control in [
        Some(QueryConsumerControl::Stop),
        Some(QueryConsumerControl::Rewind),
        None,
    ] {
        let consumer = Consumer {
            count: Cell::new(0),
            control,
        };
        let result = consumer.consume_batch(Batch::from_physical_rows(
            RowSchema::new(vec!["value".into()]),
            (1..=3)
                .map(|value| PhysicalRow::from_values(vec![Value::Int(value)]))
                .collect(),
        ));
        match control {
            Some(expected) => assert!(result.is_ok_and(|actual| actual == expected)),
            None => match result {
                Err(error) => assert_eq!(error.sqlstate(), Some("57014")),
                Ok(_) => panic!("consumer error must propagate"),
            },
        }
        assert_eq!(consumer.count.get(), 2);
    }
}
