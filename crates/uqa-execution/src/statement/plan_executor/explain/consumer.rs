//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Discard explained rows at the ordinary query consumer boundary.

use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
use std::cell::Cell;
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

pub(super) struct ExplainConsumer<'a> {
    cancellation: &'a CancellationToken,
    rows: Cell<u64>,
    serialization: Option<super::serialization::RowSerialization<'a>>,
}

impl<'a> ExplainConsumer<'a> {
    pub(super) fn new(cancellation: &'a CancellationToken) -> Self {
        Self {
            cancellation,
            rows: Cell::new(0),
            serialization: None,
        }
    }

    pub(super) fn rows(&self) -> u64 {
        self.rows.get()
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

    fn add_rows(&self, count: usize) -> Result<QueryConsumerControl, SQLError> {
        self.cancellation.check()?;
        let total = u64::try_from(count)
            .ok()
            .and_then(|count| self.rows.get().checked_add(count))
            .ok_or_else(|| SQLError::Internal("EXPLAIN ANALYZE row count exceeds u64".into()))?;
        self.rows.set(total);
        Ok(QueryConsumerControl::Continue)
    }
}

impl QueryRowConsumer for ExplainConsumer<'_> {
    fn begin(&self, columns: &[String], schema: &crate::RowSchema) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if let Some(serialization) = &self.serialization {
            serialization.begin(columns, schema)?;
        }
        Ok(())
    }

    fn consume(&self, row: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        if let Some(serialization) = &self.serialization {
            serialization.consume(&row.schema, &row.row)?;
        }
        self.add_rows(1)
    }

    fn consume_batch(&self, batch: crate::Batch) -> Result<QueryConsumerControl, SQLError> {
        if let Some(serialization) = &self.serialization {
            for row in &batch.rows {
                serialization.consume(&batch.schema, row)?;
            }
        }
        self.add_rows(batch.rows.len())
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
        let consumer = ExplainConsumer::new(&cancellation);
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
        assert_eq!(consumer.rows(), 8192);
        assert_eq!(allocation.bytes_current, 0);
        assert!(allocation.bytes_max < 256 * 1024, "{allocation:?}");
        consumer
            .consume(crate::OwnedPhysicalRow::new(
                schema,
                PhysicalRow::from_values(vec![Value::Null]),
            ))
            .unwrap();
        assert_eq!(consumer.rows(), 8193);
    }

    #[test]
    fn cancellation_and_overflow_preserve_the_last_completed_count() {
        let cancellation = CancellationToken::new();
        let consumer = ExplainConsumer::new(&cancellation);
        consumer.add_rows(7).unwrap();
        cancellation.cancel();
        assert_eq!(
            consumer.add_rows(1).err().unwrap().sqlstate(),
            Some("57014")
        );
        assert_eq!(consumer.rows(), 7);
        let cancellation = CancellationToken::new();
        let consumer = ExplainConsumer::new(&cancellation);
        consumer.rows.set(u64::MAX);
        assert!(consumer.add_rows(1).is_err());
        assert_eq!(consumer.rows(), u64::MAX);
    }
}
