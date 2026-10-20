//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded public row replay through the completed result's logical layout.

use uqa_core::Value;
use uqa_sql::SQLError;

use super::{query_output_shared, QueryOutput};
use crate::columnar_batch::result_column_positions;
use crate::physical::physical_exec_error;
use crate::spill::{SharedSpillReader, SpillRows};

pub(crate) struct PublicQueryRows {
    columns: Vec<(String, Option<usize>)>,
    rows: SpillRows<SharedSpillReader>,
    label: &'static str,
}

impl PublicQueryRows {
    pub(crate) fn new(output: QueryOutput, label: &'static str) -> Result<Self, SQLError> {
        let positions = result_column_positions(&output.internal_schema, &output.columns);
        let columns = output.columns.iter().cloned().zip(positions).collect();
        let rows = query_output_shared(output, label)?
            .read_rows()
            .map_err(physical_exec_error)?;
        Ok(Self {
            columns,
            rows,
            label,
        })
    }
}

impl Iterator for PublicQueryRows {
    type Item = Result<Vec<Value>, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|row| {
            let row = row.map_err(physical_exec_error)?;
            let view = row.view();
            self.columns
                .iter()
                .map(|(column, position)| {
                    let position = position.ok_or_else(|| {
                        SQLError::Internal(format!(
                            "{} result omitted output column `{column}`",
                            self.label
                        ))
                    })?;
                    // A valid logical NULL slot has no physical source value.
                    Ok(view.value_at(position).cloned().unwrap_or(Value::Null))
                })
                .collect()
        })
    }
}
