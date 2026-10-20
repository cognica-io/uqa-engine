//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public SQL result collection inside the original statement execution.

use crate::{
    batch::ResultRowMaterializer,
    columnar_batch::result_column_positions,
    query::{
        consumer::{QueryConsumerControl, QueryRowConsumer},
        output::{QueryOutput, ResultRows},
        statement::consumer::QueryOutputMode,
    },
    Batch, OwnedPhysicalRow, RowSchema,
};
use std::{cell::RefCell, rc::Rc};
use uqa_core::Value;
use uqa_sql::{SQLError, SQLResult};

pub(super) fn collect<'a, S: Clone + 'static>(
    execute: impl FnOnce(QueryOutputMode<'a, S>) -> Result<QueryOutput, SQLError>,
) -> Result<SQLResult, SQLError> {
    let consumer = Rc::new(PublicRows::default());
    let output = execute(QueryOutputMode::physical_consumer(consumer.clone()))?;
    let state = consumer.0.borrow_mut().take().ok_or_else(not_begun)?;
    Ok(SQLResult::from_schema_rows_with_positions(
        output.internal_schema.for_result_columns(&output.columns),
        state.rows.named,
        state.rows.positional,
    ))
}

#[derive(Default)]
struct PublicRows(RefCell<Option<State>>);

struct State {
    columns: Vec<String>,
    rows: ResultRows,
    materializer: ResultRowMaterializer,
    positions: Vec<Option<usize>>,
}

fn not_begun() -> SQLError {
    SQLError::Internal("public row collection has not begun".into())
}

impl QueryRowConsumer for PublicRows {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        let mut state = self.0.borrow_mut();
        if state.is_some() {
            return Err(SQLError::Internal(
                "public row collection began more than once".into(),
            ));
        }
        let rows = ResultRows::new(columns);
        let positions = if rows.positional.is_some() {
            result_column_positions(schema, columns)
        } else {
            Vec::new()
        };
        *state = Some(State {
            columns: columns.to_vec(),
            rows,
            materializer: ResultRowMaterializer::new(schema.clone()),
            positions,
        });
        Ok(())
    }

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let mut state = self.0.borrow_mut();
        let state = state.as_mut().ok_or_else(not_begun)?;
        if !state.materializer.matches_schema(&row.schema) {
            state.materializer = ResultRowMaterializer::new(row.schema.clone());
            if state.rows.positional.is_some() {
                state.positions = result_column_positions(&row.schema, &state.columns);
            }
        }
        if let Some(positional) = &mut state.rows.positional {
            let view = row.view();
            positional.push(
                state
                    .positions
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
        state
            .rows
            .named
            .push(state.materializer.materialize(row.row));
        Ok(QueryConsumerControl::Continue)
    }

    fn consume_batch(&self, batch: Batch) -> Result<QueryConsumerControl, SQLError> {
        let mut state = self.0.borrow_mut();
        let state = state.as_mut().ok_or_else(not_begun)?;
        state.rows.append(&state.columns, batch);
        Ok(QueryConsumerControl::Continue)
    }

    fn streams_set_inputs(&self) -> bool {
        // Changing final row ownership must preserve the statement's existing set-input evaluation order.
        false
    }
}

#[cfg(test)]
mod tests;
