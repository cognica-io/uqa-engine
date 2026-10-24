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
        let count = self.remaining.min(maximum);
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
    let result = collect_source(source).unwrap();
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
    super::collect::<()>(|mode| collect_input(source, bind_output_mode(None, mode, None)?))
}

fn collect_input(
    source: Source,
    mode: crate::query::consumer::QueryOutputMode<'_>,
) -> Result<crate::query::output::QueryOutput, uqa_sql::SQLError> {
    let runtime = QueryRuntimeView {
        diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
        cancellation: &uqa_core::CancellationToken::new(),
        settings: &Services,
        scalar_functions: &RwLock::default(),
        table_functions: &RwLock::default(),
        aggregate_functions: &RwLock::default(),
        notices: &crate::query::NoticeQueue::default(),
    };
    collect_query_operator(runtime, vec!["value".into()], Box::new(source), mode)
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
        match collect_input(source, QueryOutputMode::RowConsumer(consumer)) {
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
