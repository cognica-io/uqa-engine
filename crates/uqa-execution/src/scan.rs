//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scan operators.
//!
//! [`TableScan`] pulls rows from any [`RowSource`] in fixed-size
//! batches. The trait is the integration seam: the engine implements
//! it over its in-memory and SQLite-backed table state, the FDW layer
//! implements it over `MemoryHandler` / `DuckDBHandler` /
//! `ArrowHandler`, and tests can implement it directly over an
//! in-memory `Vec<ResultRow>`.

use crate::batch::{Batch, RowSchema, DEFAULT_BATCH_SIZE};
use crate::physical::{ExecResult, PhysicalOperator};
use uqa_sql::ResultRow;

/// Source of rows feeding a [`TableScan`]. Implementors typically own
/// a snapshot of the underlying table or external relation; the scan
/// operator holds the source as a boxed trait object so callers can
/// mix and match implementations across one query.
pub trait RowSource: Send {
    /// Stable column order for the rows produced by [`Self::next_row`].
    fn schema(&self) -> &[String];

    /// Pull the next row. Returns `None` when the source is exhausted.
    fn next_row(&mut self) -> ExecResult<Option<ResultRow>>;
}

/// In-memory source from a precomputed `Vec<ResultRow>`. Useful for
/// tests and for materialising CTE bodies.
pub struct VecSource {
    schema: Vec<String>,
    rows: std::vec::IntoIter<ResultRow>,
}

/// Physical scan over a fallible row iterator. Unlike [`VecSource`], this
/// adapter preserves producer backpressure and late errors without requiring a
/// cardinality-sized staging vector.
pub struct RowIteratorScan<'a> {
    schema: RowSchema,
    rows: Box<dyn Iterator<Item = ExecResult<ResultRow>> + Send + 'a>,
    exhausted: bool,
}

impl<'a> RowIteratorScan<'a> {
    pub fn new(
        schema: Vec<String>,
        rows: Box<dyn Iterator<Item = ExecResult<ResultRow>> + Send + 'a>,
    ) -> Self {
        Self {
            schema: RowSchema::new(schema),
            rows,
            exhausted: false,
        }
    }
}

impl PhysicalOperator for RowIteratorScan<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.exhausted = false;
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted {
            return Ok(None);
        }
        let mut batch = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        while batch.len() < DEFAULT_BATCH_SIZE {
            match self.rows.next() {
                Some(Ok(row)) => batch.push(row),
                Some(Err(error)) => return Err(error),
                None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::new(self.schema.clone(), batch)))
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.exhausted = true;
        Ok(())
    }
}

impl VecSource {
    pub fn new(schema: Vec<String>, rows: Vec<ResultRow>) -> Self {
        Self {
            schema,
            rows: rows.into_iter(),
        }
    }
}

impl RowSource for VecSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        Ok(self.rows.next())
    }
}

/// `TableScan`: a leaf operator that drains its [`RowSource`].
///
/// Emits batches of at most [`DEFAULT_BATCH_SIZE`] rows. Idempotent
/// `open` / `close` so the operator can be re-opened in tests.
pub struct TableScan {
    source: Option<Box<dyn RowSource>>,
    schema: RowSchema,
    exhausted: bool,
}

impl TableScan {
    pub fn new(source: Box<dyn RowSource>) -> Self {
        let schema = RowSchema::new(source.schema().to_vec());
        Self {
            source: Some(source),
            schema,
            exhausted: false,
        }
    }

    pub fn from_rows(schema: Vec<String>, rows: Vec<ResultRow>) -> Self {
        Self::new(Box::new(VecSource::new(schema, rows)))
    }
}

impl PhysicalOperator for TableScan {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.exhausted = false;
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted {
            return Ok(None);
        }
        let Some(src) = self.source.as_mut() else {
            return Ok(None);
        };
        let mut buf = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        for _ in 0..DEFAULT_BATCH_SIZE {
            match src.next_row()? {
                Some(row) => buf.push(row),
                None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        if buf.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::new(self.schema.clone(), buf)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.source = None;
        self.exhausted = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::run_to_rows;
    use uqa_core::Value;

    fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn table_scan_drains_source() {
        let rows = vec![
            row([("id", Value::Int(1)), ("name", Value::Str("a".into()))]),
            row([("id", Value::Int(2)), ("name", Value::Str("b".into()))]),
        ];
        let mut scan = TableScan::from_rows(vec!["id".into(), "name".into()], rows);
        let (cols, rows) = run_to_rows(&mut scan).unwrap();
        assert_eq!(cols, vec!["id", "name"]);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn table_scan_empty_source_returns_no_batch() {
        let mut scan = TableScan::from_rows(vec!["id".into()], Vec::new());
        let (_cols, rows) = run_to_rows(&mut scan).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn iterator_scan_propagates_a_late_producer_error() {
        let rows = vec![
            Ok(row([("id", Value::Int(1))])),
            Err(crate::ExecError::Other("late producer failure".into())),
        ];
        let mut scan = RowIteratorScan::new(vec!["id".into()], Box::new(rows.into_iter()));
        let error = run_to_rows(&mut scan).unwrap_err();
        assert!(error.to_string().contains("late producer failure"));
    }
}
