//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-delivery and directional-control protocol for physical query execution.

use crate::{Batch, OwnedPhysicalRow, RowSchema};
use uqa_sql::SQLError;

pub trait QueryRowConsumer {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError>;

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError>;

    fn consume_batch(&self, batch: Batch) -> Result<QueryConsumerControl, SQLError> {
        for row in batch.rows {
            let control = self.consume(OwnedPhysicalRow::new(batch.schema.clone(), row))?;
            if control != QueryConsumerControl::Continue {
                return Ok(control);
            }
        }
        Ok(QueryConsumerControl::Continue)
    }

    /// Requested output rows before this consumer receives control. The default retains row-level demand for stop and error boundaries. A consumer that accepts complete input may return `None` to retain the producer's ordinary bounded batches.
    fn input_demand(&self) -> Option<std::num::NonZeroUsize> {
        Some(crate::physical::demand::ONE)
    }

    /// Whether UNION ALL may deliver directly from each child instead of its ordinary set operator.
    fn streams_set_inputs(&self) -> bool {
        true
    }

    fn uses_directional_scan(&self) -> bool {
        false
    }

    fn directional_scan_prepared(
        &self,
        _support: crate::BackwardScanSupport,
    ) -> Result<(), SQLError> {
        Ok(())
    }

    fn scan_direction(&self) -> crate::PhysicalScanDirection {
        crate::PhysicalScanDirection::Forward
    }

    fn direction_exhausted(&self) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Stop)
    }

    fn rewound(&self) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Continue)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QueryConsumerControl {
    Continue,
    Stop,
    Rewind,
}

#[derive(Clone)]
pub enum QueryOutputMode<'a> {
    Rows,
    SharedSpill,
    ExistsKeySet(Option<&'a dyn uqa_sql::expr::SQLValueCatalog>),
    RowConsumer(std::rc::Rc<dyn QueryRowConsumer + 'a>),
}
