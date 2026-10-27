//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public SQL result collection inside the original statement execution.

use crate::{
    query::{
        consumer::{QueryConsumerControl, QueryRowConsumer},
        output::{QueryOutput, ResultRows},
        statement::consumer::QueryOutputMode,
    },
    Batch, OwnedPhysicalRow, RowSchema,
};
use std::{cell::RefCell, rc::Rc};
use uqa_sql::{SQLError, SQLResult};

pub(super) fn collect<'a, S: Clone + 'static>(
    execute: impl FnOnce(QueryOutputMode<'a, S>) -> Result<QueryOutput, SQLError>,
) -> Result<SQLResult, SQLError> {
    collect_measured(None, execute)
}

pub(super) fn collect_measured<'a, S: Clone + 'static>(
    measurement: Option<&'a crate::query::measurement::QueryMeasurement>,
    execute: impl FnOnce(QueryOutputMode<'a, S>) -> Result<QueryOutput, SQLError>,
) -> Result<SQLResult, SQLError> {
    let consumer = Rc::new(PublicRows {
        state: RefCell::default(),
        measurement,
    });
    let execute = || execute(QueryOutputMode::physical_consumer(consumer.clone()));
    let output = match measurement {
        Some(measurement) => measurement.run(execute),
        None => execute(),
    }?;
    let state = consumer.state.borrow_mut().take().ok_or_else(not_begun)?;
    Ok(SQLResult::from_schema_rows_with_positions(
        output.internal_schema.for_result_columns(&output.columns),
        state.rows.named,
        state.rows.positional,
    ))
}

#[derive(Default)]
struct PublicRows<'a> {
    state: RefCell<Option<State>>,
    measurement: Option<&'a crate::query::measurement::QueryMeasurement>,
}

impl PublicRows<'_> {
    fn receive<T>(
        &self,
        rows: usize,
        consume: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        match self.measurement {
            Some(measurement) => measurement.output(rows, consume),
            None => consume(),
        }
    }
}

struct State {
    columns: Vec<String>,
    schema: RowSchema,
    rows: ResultRows,
}

fn not_begun() -> SQLError {
    SQLError::Internal("public row collection has not begun".into())
}

impl QueryRowConsumer for PublicRows<'_> {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        self.receive(0, || {
            let mut state = self.state.borrow_mut();
            if state.is_some() {
                return Err(SQLError::Internal(
                    "public row collection began more than once".into(),
                ));
            }
            let mut rows = ResultRows::new(columns);
            rows.prepare(columns, schema);
            *state = Some(State {
                columns: columns.to_vec(),
                schema: schema.clone(),
                rows,
            });
            Ok(())
        })
    }

    fn collect_native_materialized(
        &self,
        operator: &mut dyn crate::PhysicalOperator,
        cancellation: &uqa_core::CancellationToken,
    ) -> crate::ExecResult<bool> {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().ok_or_else(not_begun)?;
        let mut sink = crate::query::output::MaterializedResultSink::new(
            &state.schema,
            &mut state.rows,
            cancellation,
        )
        .with_measurement(self.measurement);
        operator.collect_materialized(&mut sink)
    }

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        self.receive(1, || {
            let mut state = self.state.borrow_mut();
            let state = state.as_mut().ok_or_else(not_begun)?;
            state.rows.append_row(&state.columns, row);
            Ok(QueryConsumerControl::Continue)
        })
    }

    fn consume_batch(&self, batch: Batch) -> Result<QueryConsumerControl, SQLError> {
        self.receive(batch.rows.len(), || {
            let mut state = self.state.borrow_mut();
            let state = state.as_mut().ok_or_else(not_begun)?;
            state.rows.append(&state.columns, batch);
            Ok(QueryConsumerControl::Continue)
        })
    }

    fn input_demand(&self) -> Option<std::num::NonZeroUsize> {
        None
    }

    fn streams_set_inputs(&self) -> bool {
        // Changing final row ownership must preserve the statement's existing set-input evaluation order.
        false
    }
}

#[cfg(test)]
mod tests;
