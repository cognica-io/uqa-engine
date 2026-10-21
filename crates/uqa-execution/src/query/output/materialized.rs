//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final public rows consume each physical batch without retaining a second complete input.

use crate::{
    batch::ResultRowMaterializer, columnar_batch::result_column_positions, Batch, OwnedPhysicalRow,
    RowSchema, RowSchemaExecution,
};
use uqa_core::Value;
use uqa_sql::ResultRow;

pub(crate) struct ResultRows {
    pub(crate) named: Vec<ResultRow>,
    pub(crate) positional: Option<Vec<Vec<Value>>>,
    materializer: Option<ResultRowMaterializer>,
    positions: Vec<Option<usize>>,
}

impl ResultRows {
    pub(crate) fn new(columns: &[String]) -> Self {
        let mut labels = std::collections::BTreeSet::new();
        let duplicates = columns.iter().any(|column| !labels.insert(column));
        Self {
            named: Vec::new(),
            positional: duplicates.then(Vec::new),
            materializer: None,
            positions: Vec::new(),
        }
    }

    pub(crate) fn prepare(&mut self, columns: &[String], schema: &RowSchema) {
        if self
            .materializer
            .as_ref()
            .is_none_or(|materializer| !materializer.matches_schema(schema))
        {
            self.materializer = Some(ResultRowMaterializer::new(schema.clone()));
            if self.positional.is_some() {
                self.positions = result_column_positions(schema, columns);
            }
        }
    }

    pub(crate) fn append_row(&mut self, columns: &[String], row: OwnedPhysicalRow) {
        self.prepare(columns, &row.schema);
        if let Some(positional) = &mut self.positional {
            let view = row.view();
            positional.push(
                self.positions
                    .iter()
                    .map(|position| {
                        position
                            .and_then(|position| view.value_at(position))
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            );
        }
        self.named.push(
            self.materializer
                .as_ref()
                .expect("row layout was prepared")
                .materialize(row.row),
        );
    }

    pub(crate) fn append(&mut self, columns: &[String], batch: Batch) {
        self.prepare(columns, &batch.schema);
        if let Some(positional) = &mut self.positional {
            positional.extend(batch.rows.iter().map(|row| {
                let view = batch.schema.view(row);
                self.positions
                    .iter()
                    .map(|position| {
                        position
                            .and_then(|position| view.value_at(position))
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect()
            }));
        }
        self.materializer
            .as_ref()
            .expect("batch layout was prepared")
            .extend(&mut self.named, batch.rows);
    }
}
