//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{OwnedPhysicalRow, PhysicalRow, ResultRow, RowFragment, RowSchema, DEFAULT_BATCH_SIZE};

/// A schema and bounded vector of physical rows flowing between operators.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub schema: RowSchema,
    pub rows: Vec<PhysicalRow>,
}

impl Batch {
    /// Compatibility constructor for named rows entering the physical engine. The resulting batch is positional immediately; maps do not flow to the next operator.
    pub fn new(schema: RowSchema, rows: Vec<ResultRow>) -> Self {
        let rows = rows
            .into_iter()
            .map(|row| PhysicalRow::from_result_row(&schema, row))
            .collect();
        Self { schema, rows }
    }

    pub fn from_physical_rows(schema: RowSchema, rows: Vec<PhysicalRow>) -> Self {
        debug_assert!(rows.iter().all(|row| {
            row.fragments.iter().map(RowFragment::len).sum::<usize>() == schema.physical_width()
        }));
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

    pub fn into_result_rows(self) -> Vec<ResultRow> {
        let schema = self.schema;
        if schema.index.cold.identity_layout {
            return self
                .rows
                .into_iter()
                .map(|row| schema.materialize_identity_result_row(row))
                .collect();
        }
        schema.materialize_remapped_result_rows(self.rows)
    }

    /// Consume a batch without materializing named maps.
    pub fn into_owned_rows(self) -> Vec<OwnedPhysicalRow> {
        let schema = self.schema;
        self.rows
            .into_iter()
            .map(|row| OwnedPhysicalRow::new(schema.clone(), row))
            .collect()
    }

    /// Split named rows into batches of at most [`DEFAULT_BATCH_SIZE`].
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
