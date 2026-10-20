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
