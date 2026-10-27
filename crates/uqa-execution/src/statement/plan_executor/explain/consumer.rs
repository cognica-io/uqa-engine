//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Discard explained rows at the ordinary query consumer boundary.

use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

pub(super) struct ExplainConsumer<'a> {
    cancellation: &'a CancellationToken,
    measurement: &'a crate::query::measurement::QueryMeasurement,
    serialization: Option<super::serialization::RowSerialization<'a>>,
}

impl<'a> ExplainConsumer<'a> {
    pub(super) fn new(
        cancellation: &'a CancellationToken,
        measurement: &'a crate::query::measurement::QueryMeasurement,
    ) -> Self {
        Self {
            cancellation,
            measurement,
            serialization: None,
        }
    }

    pub(super) fn with_serialization(
        mut self,
        serialization: Option<super::serialization::RowSerialization<'a>>,
    ) -> Self {
        self.serialization = serialization;
        self
    }

    pub(super) fn serialization(&self) -> uqa_sql::result::explain::ExplainSerializationMetrics {
        self.serialization
            .as_ref()
            .map(super::serialization::RowSerialization::metrics)
            .unwrap_or_default()
    }
}

impl QueryRowConsumer for ExplainConsumer<'_> {
    fn begin(&self, columns: &[String], schema: &crate::RowSchema) -> Result<(), SQLError> {
        self.cancellation.check()?;
        self.measurement.output(0, || {
            if let Some(serialization) = &self.serialization {
                serialization.begin(columns, schema)?;
            }
            Ok(())
        })
    }

    fn consume(&self, row: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        self.cancellation.check()?;
        self.measurement.output(1, || {
            if let Some(serialization) = &self.serialization {
                serialization.consume(&row.schema, &row.row)?;
            }
            Ok(QueryConsumerControl::Continue)
        })
    }

    fn consume_batch(&self, batch: crate::Batch) -> Result<QueryConsumerControl, SQLError> {
        self.cancellation.check()?;
        self.measurement.output(batch.rows.len(), || {
            if let Some(serialization) = &self.serialization {
                for row in &batch.rows {
                    serialization.consume(&batch.schema, row)?;
                }
            }
            Ok(QueryConsumerControl::Continue)
        })
    }

    fn input_demand(&self) -> Option<std::num::NonZeroUsize> {
        self.serialization
            .as_ref()
            .map(|_| crate::physical::demand::ONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Batch, PhysicalRow, RowSchema};
    use uqa_core::Value;

    #[test]
    fn batches_and_rows_contribute_once_without_retaining_their_values() {
        let cancellation = CancellationToken::new();
        let measurement = crate::query::measurement::QueryMeasurement::new(
            false,
            &uqa_core::memory::MemoryBudget::new(65_536),
        );
        let consumer = ExplainConsumer::new(&cancellation, &measurement);
        measurement
            .run(|| {
                let schema = RowSchema::new(vec!["value".into()]);
                consumer.begin(schema.columns(), &schema).unwrap();
                let allocation = allocation_counter::measure(|| {
                    for _ in 0..256 {
                        let rows = (0..32)
                            .map(|_| PhysicalRow::from_values(vec![Value::Str("x".repeat(4096))]))
                            .collect();
                        consumer
                            .consume_batch(Batch::from_physical_rows(schema.clone(), rows))
                            .unwrap();
                    }
                });
                assert_eq!(allocation.bytes_current, 0);
                assert!(allocation.bytes_max < 256 * 1024, "{allocation:?}");
                consumer
                    .consume(crate::OwnedPhysicalRow::new(
                        schema,
                        PhysicalRow::from_values(vec![Value::Null]),
                    ))
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(measurement.metrics().rows, 8193);
        assert_eq!(measurement.metrics().loops, 1);
    }

    #[test]
    fn cancellation_preserves_the_last_produced_count() {
        let cancellation = CancellationToken::new();
        let measurement = crate::query::measurement::QueryMeasurement::new(
            false,
            &uqa_core::memory::MemoryBudget::new(65_536),
        );
        let consumer = ExplainConsumer::new(&cancellation, &measurement);
        let row = || {
            crate::OwnedPhysicalRow::new(
                RowSchema::new(vec!["v".into()]),
                crate::PhysicalRow::from_values(vec![Value::Int(1)]),
            )
        };
        let error = measurement
            .run(|| {
                consumer.consume(row())?;
                cancellation.cancel();
                consumer.consume(row())
            })
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(measurement.metrics().rows, 1);
    }
}
