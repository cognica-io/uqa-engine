//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final public rows consume each physical batch without retaining a second complete input.

use crate::{Batch, ColumnarBatch};
use uqa_core::Value;
use uqa_sql::ResultRow;

pub(super) struct ResultRows {
    pub(super) named: Vec<ResultRow>,
    pub(super) positional: Option<Vec<Vec<Value>>>,
}

impl ResultRows {
    pub(super) fn new(columns: &[String]) -> Self {
        let mut labels = std::collections::BTreeSet::new();
        let duplicates = columns.iter().any(|column| !labels.insert(column));
        Self {
            named: Vec::new(),
            positional: duplicates.then(Vec::new),
        }
    }

    pub(super) fn append(&mut self, columns: &[String], batch: Batch) {
        if let Some(positional) = &mut self.positional {
            let rows = ColumnarBatch::from_borrowed_batch(columns, &batch).into_positional_rows();
            if positional.is_empty() {
                *positional = rows;
            } else {
                positional.extend(rows);
            }
        }
        let named = batch.into_result_rows();
        if self.named.is_empty() {
            self.named = named;
        } else {
            self.named.extend(named);
        }
    }
}
