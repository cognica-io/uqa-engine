//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row batch: a small, schema-aware bag of rows that flows between
//! Volcano operators.
//!
//! A [`Batch`] carries a schema (column names) and a vector of
//! [`uqa_sql::ResultRow`]. The default batch size mirrors the Python
//! reference (1024 rows). Operators are free to emit smaller or larger
//! batches; callers must not rely on a fixed size.

use uqa_sql::ResultRow;

/// Default rows-per-batch hint. Mirrors `DEFAULT_BATCH_SIZE` in
/// `uqa/execution/batch.py`.
pub const DEFAULT_BATCH_SIZE: usize = 1024;

/// Column-name schema. Equality is positional, like SQL projections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowSchema {
    pub columns: Vec<String>,
}

impl RowSchema {
    pub fn new(columns: Vec<String>) -> Self {
        Self { columns }
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.columns.iter()
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
}

/// A single batch of rows that travels through the pipeline. The
/// schema is duplicated on every batch so consumers do not need to
/// remember which operator produced them; this matches the Arrow
/// `RecordBatch` contract.
#[derive(Debug, Clone)]
pub struct Batch {
    pub schema: RowSchema,
    pub rows: Vec<ResultRow>,
}

impl Batch {
    pub fn new(schema: RowSchema, rows: Vec<ResultRow>) -> Self {
        Self { schema, rows }
    }

    pub fn empty(schema: RowSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Split a row vector into batches of at most [`DEFAULT_BATCH_SIZE`].
    pub fn chunked(schema: RowSchema, rows: Vec<ResultRow>) -> Vec<Batch> {
        if rows.is_empty() {
            return vec![Batch::empty(schema)];
        }
        let mut out = Vec::with_capacity(rows.len().div_ceil(DEFAULT_BATCH_SIZE));
        let mut buf = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        for row in rows {
            buf.push(row);
            if buf.len() == DEFAULT_BATCH_SIZE {
                out.push(Batch::new(schema.clone(), std::mem::take(&mut buf)));
                buf.reserve(DEFAULT_BATCH_SIZE);
            }
        }
        if !buf.is_empty() {
            out.push(Batch::new(schema, buf));
        }
        out
    }
}
