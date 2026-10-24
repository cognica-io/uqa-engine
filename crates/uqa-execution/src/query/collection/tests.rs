//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{collect_materialized_rows, QueryRows};
use crate::{Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, RowSchema};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;
use uqa_sql::SQLError;

struct Source {
    schema: RowSchema,
    row: PhysicalRow,
    remaining: usize,
    emitted: usize,
    fail_after: Option<usize>,
    closes: Arc<AtomicUsize>,
}

impl Source {
    fn new(count: usize) -> Self {
        Self {
            schema: RowSchema::new(vec!["value".into()]),
            row: PhysicalRow::from_values(vec![Value::Str("λ".repeat(8192))]),
            remaining: count,
            emitted: 0,
            fail_after: None,
            closes: Arc::default(),
        }
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
        if self.fail_after.is_some_and(|limit| self.emitted >= limit) {
            return Err(ExecError::SQL(SQLError::Routine {
                sqlstate: "22012".into(),
                message: "late input failure".into(),
            }));
        }
        let count = self.remaining.min(128);
        self.remaining -= count;
        self.emitted += count;
        Ok((count != 0)
            .then(|| Batch::from_physical_rows(self.schema.clone(), vec![self.row.clone(); count])))
    }
    fn close(&mut self) -> ExecResult<()> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn materialized_collection_retains_one_output_vector_and_one_input_batch() {
    const COUNT: usize = 8192;
    let mut source = Source::new(COUNT);
    let measured = allocation_counter::measure(|| {
        let QueryRows::Rows(rows) = collect_materialized_rows(&mut source).unwrap() else {
            panic!("physical rows must preserve their layout");
        };
        assert_eq!(rows.len(), COUNT);
        for row in &rows {
            assert!(matches!(row.value(0), Some(Value::Str(value)) if value.len() == 16384));
        }
    });
    // During the last geometric growth, the allocator briefly owns the old half-capacity vector as well as the new output allocation.
    let limit =
        u64::try_from((COUNT + COUNT / 2 + 128) * size_of::<PhysicalRow>() + 64 * 1024).unwrap();
    assert!(measured.bytes_max <= limit, "{measured:?}; limit {limit}");
    assert_eq!(source.closes.load(Ordering::Relaxed), 1);
}

#[test]
fn materialized_collection_preserves_empty_and_late_error_cleanup() {
    let mut empty = Source::new(0);
    let QueryRows::Rows(rows) = collect_materialized_rows(&mut empty).unwrap() else {
        panic!("physical rows");
    };
    assert!(rows.is_empty());
    assert_eq!(empty.closes.load(Ordering::Relaxed), 1);
    let mut failing = Source::new(256);
    failing.fail_after = Some(128);
    let error = match collect_materialized_rows(&mut failing) {
        Err(error) => error,
        Ok(_) => panic!("later source failure must discard the partial result"),
    };
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(failing.closes.load(Ordering::Relaxed), 1);
}

#[test]
fn rejected_consumer_preparation_closes_the_unopened_input() {
    use super::super::consumer::{QueryConsumerControl, QueryRowConsumer};
    use std::rc::Rc;

    struct Input {
        schema: RowSchema,
        closed: Arc<AtomicUsize>,
    }
    impl PhysicalOperator for Input {
        fn row_schema(&self) -> &RowSchema {
            &self.schema
        }
        fn open(&mut self) -> ExecResult<()> {
            panic!("rejected consumer must not open input")
        }
        fn next(&mut self) -> ExecResult<Option<Batch>> {
            panic!("rejected consumer must not read")
        }
        fn close(&mut self) -> ExecResult<()> {
            assert_eq!(self.closed.fetch_add(1, Ordering::SeqCst), 0);
            Err(SQLError::Routine {
                sqlstate: "58030".into(),
                message: "input close failed".into(),
            }
            .into())
        }
    }
    struct Reject {
        directional: bool,
        reject_preparation: bool,
    }
    impl QueryRowConsumer for Reject {
        fn begin(&self, _: &[String], _: &RowSchema) -> Result<(), SQLError> {
            Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "consumer admission failed".into(),
            })
        }
        fn consume(&self, _: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
            panic!("no rows")
        }
        fn uses_directional_scan(&self) -> bool {
            self.directional
        }
        fn directional_scan_prepared(&self, _: crate::BackwardScanSupport) -> Result<(), SQLError> {
            if self.reject_preparation {
                self.begin(&[], &RowSchema::default())
            } else {
                Ok(())
            }
        }
    }
    for (directional, reject_preparation) in [(false, false), (true, false), (true, true)] {
        let closed = Arc::new(AtomicUsize::new(0));
        let schema = RowSchema::new(vec!["n".into()]);
        let mut input: Box<dyn PhysicalOperator> = Box::new(Input {
            schema: schema.clone(),
            closed: closed.clone(),
        });
        let consumer: Rc<dyn QueryRowConsumer> = Rc::new(Reject {
            directional,
            reject_preparation,
        });
        let result = super::collect_consumed_rows(
            schema.columns(),
            &schema,
            &mut input,
            &consumer,
            &uqa_core::CancellationToken::new(),
        );
        let Err(error) = result else {
            panic!("consumer was accepted")
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(error.to_string(), "consumer admission failed");
        assert!(error.detail().unwrap().contains("input close failed"));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
