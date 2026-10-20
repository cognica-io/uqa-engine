//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve INSERT source completion while reading only one bounded row at a time.

use std::borrow::Cow;

use uqa_core::CancellationToken;
use uqa_sql::{SQLError, ScalarExpr};

use crate::physical::physical_exec_error;
use crate::query::output::{query_output_shared, QueryOutput};
use crate::spill::{SharedSpillReader, SpillRows};

pub(crate) enum InsertInputRows<'a> {
    Values(std::slice::Iter<'a, Vec<ScalarExpr>>),
    Query {
        columns: Vec<String>,
        rows: Box<SpillRows<SharedSpillReader>>,
        cancellation: CancellationToken,
    },
}

impl<'a> InsertInputRows<'a> {
    pub(crate) fn values(rows: &'a [Vec<ScalarExpr>]) -> Self {
        Self::Values(rows.iter())
    }

    pub(crate) fn query(
        output: QueryOutput,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        let columns = output.columns.clone();
        let retained = query_output_shared(output, "INSERT SELECT")?;
        let rows = retained.read_rows().map_err(physical_exec_error)?;
        Ok(Self::Query {
            columns,
            rows: Box::new(rows),
            cancellation,
        })
    }
}

impl<'a> Iterator for InsertInputRows<'a> {
    type Item = Result<Cow<'a, [ScalarExpr]>, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Values(rows) => rows.next().map(|row| Ok(Cow::Borrowed(row.as_slice()))),
            Self::Query {
                columns,
                rows,
                cancellation,
            } => {
                if let Err(error) = cancellation.check() {
                    return Some(Err(error.into()));
                }
                rows.next().map(|row| {
                    let row = row.map_err(physical_exec_error)?;
                    let mut values = row.row.into_physical_values().into_iter();
                    columns
                        .iter()
                        .map(|column| {
                            values.next().map(ScalarExpr::Literal).ok_or_else(|| {
                                SQLError::Internal(format!(
                                    "INSERT SELECT result omitted output column `{column}`"
                                ))
                            })
                        })
                        .collect::<Result<Vec<_>, SQLError>>()
                        .map(Cow::Owned)
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
