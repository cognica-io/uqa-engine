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

use crate::batch::{Batch, PhysicalRow, RowSchema, DEFAULT_BATCH_SIZE};
use crate::physical::{ExecResult, PhysicalOperator, PhysicalOrder};
use uqa_sql::ResultRow;

/// Source of rows feeding a [`TableScan`]. Implementors typically own
/// a snapshot of the underlying table or external relation; the scan
/// operator holds the source as a boxed trait object so callers can
/// mix and match implementations across one query.
pub trait RowSource: Send {
    /// Stable column order for the rows produced by [`Self::next_row`].
    fn schema(&self) -> &[String];

    /// Optional non-identity schema used by positional sources. This carries
    /// hidden lookup aliases and slot remaps that cannot be represented by the
    /// legacy column-name slice.
    fn physical_schema(&self) -> Option<&RowSchema> {
        None
    }

    /// Estimated total rows available from this source.
    fn estimated_cardinality(&self) -> Option<u64> {
        None
    }

    /// Leading row order guaranteed by the source.
    fn output_ordering(&self) -> &[PhysicalOrder] {
        &[]
    }

    /// Pull the next row. Returns `None` when the source is exhausted.
    fn next_row(&mut self) -> ExecResult<Option<ResultRow>>;

    /// Pull up to `max_rows` without forcing batch-capable sources through a
    /// row-at-a-time lock or backend call. The default preserves compatibility
    /// for iterator-like sources.
    fn next_batch(&mut self, max_rows: usize) -> ExecResult<Vec<ResultRow>> {
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            match self.next_row()? {
                Some(row) => rows.push(row),
                None => break,
            }
        }
        Ok(rows)
    }

    /// Pull a positional batch directly. Backend-native sources override this
    /// to avoid constructing named maps at the scan boundary; compatibility
    /// sources are converted exactly once here.
    fn next_physical_batch(&mut self, max_rows: usize) -> ExecResult<Vec<PhysicalRow>> {
        let schema = RowSchema::new(self.schema().to_vec());
        self.next_batch(max_rows).map(|rows| {
            rows.into_iter()
                .map(|row| PhysicalRow::from_result_row(&schema, row))
                .collect()
        })
    }

    /// Feed backend-native projected rows directly to an aggregate. Sources
    /// that cannot preserve their normal filter and virtual-column semantics
    /// return `false` without advancing their cursor.
    fn consume_into_aggregate(
        &mut self,
        _executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        Ok(false)
    }
}

/// In-memory source from a precomputed `Vec<ResultRow>`. Useful for
/// tests and for materialising CTE bodies.
pub struct VecSource {
    schema: Vec<String>,
    physical_schema: RowSchema,
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
    fn row_schema(&self) -> &RowSchema {
        &self.schema
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
        let physical_schema = RowSchema::from_named_columns(schema.clone());
        Self {
            schema,
            physical_schema,
            rows: rows.into_iter(),
        }
    }
}

impl RowSource for VecSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        u64::try_from(self.rows.len()).ok()
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
    ordering: Vec<PhysicalOrder>,
    estimated_cardinality: Option<u64>,
    exhausted: bool,
}

impl TableScan {
    pub fn new(source: Box<dyn RowSource>) -> Self {
        let schema = source
            .physical_schema()
            .cloned()
            .unwrap_or_else(|| RowSchema::new(source.schema().to_vec()));
        let ordering = source.output_ordering().to_vec();
        let estimated_cardinality = source.estimated_cardinality();
        Self {
            source: Some(source),
            schema,
            ordering,
            estimated_cardinality,
            exhausted: false,
        }
    }

    pub fn from_rows(schema: Vec<String>, rows: Vec<ResultRow>) -> Self {
        Self::new(Box::new(VecSource::new(schema, rows)))
    }
}

impl PhysicalOperator for TableScan {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.estimated_cardinality
    }

    fn output_ordering(&self) -> &[PhysicalOrder] {
        &self.ordering
    }

    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        let Some(source) = self.source.as_mut() else {
            return Ok(false);
        };
        source.consume_into_aggregate(executor)
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
        let buf = src.next_physical_batch(DEFAULT_BATCH_SIZE)?;
        if buf.is_empty() {
            self.exhausted = true;
            return Ok(None);
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)))
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
