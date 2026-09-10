//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-delivery and directional-control protocol for physical query execution.

use crate::{OwnedPhysicalRow, RowSchema};
use uqa_sql::SQLError;

pub trait QueryRowConsumer {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError>;

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError>;

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
    ExistsKeySet,
    RowConsumer(std::rc::Rc<dyn QueryRowConsumer + 'a>),
}
