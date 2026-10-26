//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe native lateral row streams without changing their execution protocol.

use super::{instance::Instance, Event};
use crate::{
    physical::physical_exec_error, query::diagnostics::QueryDiagnostics, SQLRowSource, SQLRowStream,
};
use uqa_sql::SQLError;

struct ObservedRows<'a, T> {
    rows: SQLRowStream<'a, T>,
    instance: Instance<'a>,
}

impl QueryDiagnostics {
    pub(crate) fn observe_rows<'a, T: 'a>(
        &'a self,
        mut rows: SQLRowStream<'a, T>,
        kind: &'static str,
    ) -> Result<SQLRowStream<'a, T>, SQLError> {
        let instance = match self.instance(kind, size_of::<ObservedRows<'_, T>>()) {
            Ok(instance) => instance,
            Err(error) => return Err(rows.fail(error)),
        };
        Ok(match instance {
            Some(instance) => SQLRowStream::from_source(ObservedRows { rows, instance }),
            None => rows,
        })
    }
}

impl<T> SQLRowSource<T> for ObservedRows<'_, T> {
    fn next_row(&mut self) -> Result<Option<T>, SQLError> {
        self.instance
            .perform(
                &mut self.rows,
                |rows, step| {
                    let row = rows.next().transpose()?;
                    if let Some(step) = step {
                        if row.is_some() {
                            step.ready(1)?;
                        }
                    }
                    Ok(row)
                },
                |_, step| Event::Consume(step),
                None,
            )
            .map_err(physical_exec_error)
    }
    fn close(&mut self) -> Result<(), SQLError> {
        self.instance
            .perform(
                &mut self.rows,
                |rows, _| Ok(rows.close()?),
                |_, step| Event::Close(step),
                Some(|rows| Ok(rows.close()?)),
            )
            .map_err(physical_exec_error)
    }
}
