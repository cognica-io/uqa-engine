//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded result cursors backed by an owned spill buffer.

use crate::{ColumnarBatch, SharedSpill, SharedSpillReader};
use uqa_sql::{ColumnType, SQLError};

/// Metadata known before a cursor is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLCursorSummary {
    pub columns: Vec<String>,
    pub column_types: Vec<Option<ColumnType>>,
    pub row_count: usize,
    pub spilled_to_disk: bool,
}

/// Iterator over schema-ordered column batches backed by a work-mem-bounded
/// [`SharedSpill`]. Dropping the cursor releases its temporary file. Positional
/// conversion preserves separately-valued duplicate labels in [`ColumnarBatch`].
pub struct SQLCursor {
    summary: SQLCursorSummary,
    reader: SharedSpillReader,
}

impl SQLCursor {
    pub fn from_spill(
        columns: Vec<String>,
        column_types: Vec<Option<ColumnType>>,
        spill: SharedSpill,
    ) -> Result<Self, SQLError> {
        debug_assert_eq!(columns.len(), column_types.len());
        let summary = SQLCursorSummary {
            columns,
            column_types,
            row_count: spill.rows(),
            spilled_to_disk: spill.has_spilled(),
        };
        let reader = spill
            .into_reader()
            .map_err(crate::physical::physical_exec_error)?;
        Ok(Self { summary, reader })
    }

    pub fn columns(&self) -> &[String] {
        &self.summary.columns
    }

    pub fn column_types(&self) -> &[Option<ColumnType>] {
        &self.summary.column_types
    }

    pub fn row_count(&self) -> usize {
        self.summary.row_count
    }

    pub fn spilled_to_disk(&self) -> bool {
        self.summary.spilled_to_disk
    }

    pub fn summary(&self) -> SQLCursorSummary {
        self.summary.clone()
    }
}

impl Iterator for SQLCursor {
    type Item = Result<ColumnarBatch, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let batch = match self.reader.next()? {
                Ok(batch) => batch,
                Err(error) => return Some(Err(crate::physical::physical_exec_error(error))),
            };
            if !batch.is_empty() {
                return Some(Ok(ColumnarBatch::from_batch(&self.summary.columns, batch)));
            }
        }
    }
}
