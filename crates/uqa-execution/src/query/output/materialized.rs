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

/// Native final-result accumulator. Its only row operation copies selected values into the public result and cannot invoke a host callback or reenter storage.
pub struct MaterializedResultSink<'a> {
    schema: &'a RowSchema,
    rows: &'a mut ResultRows,
    cancellation: &'a uqa_core::CancellationToken,
    measurement: Option<&'a crate::query::measurement::QueryMeasurement>,
}

impl<'a> MaterializedResultSink<'a> {
    pub(crate) fn new(
        schema: &'a RowSchema,
        rows: &'a mut ResultRows,
        cancellation: &'a uqa_core::CancellationToken,
    ) -> Self {
        Self {
            schema,
            rows,
            cancellation,
            measurement: None,
        }
    }

    pub(crate) fn with_measurement(
        mut self,
        measurement: Option<&'a crate::query::measurement::QueryMeasurement>,
    ) -> Self {
        self.measurement = measurement;
        self
    }

    /// Check the original statement token between bounded input pages, including pages rejected by the predicate.
    pub fn check_cancelled(&self) -> crate::ExecResult<()> {
        self.cancellation.check().map_err(uqa_sql::SQLError::from)?;
        Ok(())
    }

    #[must_use]
    pub fn physical_width(&self) -> usize {
        self.schema.physical_width()
    }

    /// Consume one complete physical row during its storage borrow. No borrowed value survives this call.
    pub fn append(&mut self, values: &[&Value]) -> crate::ExecResult<()> {
        if values.len() != self.physical_width() {
            return Err(crate::ExecError::Other(
                "borrowed materialized row has an incompatible physical width".into(),
            ));
        }
        if let Some(measurement) = self.measurement {
            measurement.output(1, || {
                self.append_row(values);
                Ok(())
            })?;
        } else {
            self.append_row(values);
        }
        Ok(())
    }

    fn append_row(&mut self, values: &[&Value]) {
        if let Some(positional) = &mut self.rows.positional {
            positional.push(
                self.rows
                    .positions
                    .iter()
                    .map(|position| {
                        position
                            .and_then(|logical| self.schema.physical_slot(logical))
                            .and_then(|slot| values.get(slot).copied())
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            );
        }
        self.rows.named.push(
            self.rows
                .materializer
                .as_ref()
                .expect("public result layout was prepared")
                .materialize_borrowed(values),
        );
    }
}

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
