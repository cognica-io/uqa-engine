//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-function rows whose call starts at the first row request, as `PostgreSQL`'s function scan evaluates its call only when it is first asked for a row.

use crate::{PhysicalProjectRows, PhysicalRow};
use uqa_sql::SQLError;

type StartCall<'a> = Box<dyn FnOnce() -> Result<PhysicalProjectRows, SQLError> + Send + 'a>;

/// The rows of a table-function call that has not started yet. The first row request evaluates the call's arguments and starts its stream; a consumer that requests no row never evaluates the call.
pub struct DeferredTableFunctionRows<'a> {
    start: Option<StartCall<'a>>,
    rows: Option<PhysicalProjectRows>,
}

impl<'a> DeferredTableFunctionRows<'a> {
    pub fn new(start: impl FnOnce() -> Result<PhysicalProjectRows, SQLError> + Send + 'a) -> Self {
        Self {
            start: Some(Box::new(start)),
            rows: None,
        }
    }
}

impl crate::SQLRowSource<PhysicalRow> for DeferredTableFunctionRows<'_> {
    fn next_row(&mut self) -> Result<Option<PhysicalRow>, SQLError> {
        if let Some(start) = self.start.take() {
            match start() {
                Ok(rows) => self.rows = Some(rows),
                Err(error) => return Err(error),
            }
        }
        self.rows
            .as_mut()
            .map_or(Ok(None), |rows| rows.next().transpose())
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.start = None;
        self.rows.take().map_or(Ok(()), |mut rows| rows.close())
    }
}

#[cfg(test)]
mod tests;
