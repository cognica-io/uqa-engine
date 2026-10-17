//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning regular-expression functions drawn one row at a time from the SQL regular-expression owner.

use super::TableFunctionRows;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{expr::RegexMatches, SQLError};

/// Stream `regexp_matches(string, pattern [, flags])` as one `text[]` column. The cursor owns its input copy and compiled pattern and finds each match only when its row is requested, so one row is resident at a time; a NULL argument yields no rows. Allocation follows ordinary scalar evaluation, because `PostgreSQL`'s `work_mem` bounds sort and hash workspaces rather than function results, and the stream's caller checks cancellation before each row.
pub(super) fn regexp_matches_row_stream(
    evaluated: &[Value],
    column: String,
) -> Result<TableFunctionRows, SQLError> {
    let mut cursor = RegexMatches::new(evaluated, &ProductionControl::uncontrolled())?;
    Ok(TableFunctionRows::new(
        vec![column],
        Box::new(std::iter::from_fn(move || {
            let matches = cursor.as_mut()?;
            match matches.next_row(&ProductionControl::uncontrolled()) {
                Ok(Some(row)) => Some(Ok(crate::PhysicalRow::from_values(vec![
                    row.into_parts().0,
                ]))),
                Ok(None) => {
                    cursor = None;
                    None
                }
                Err(error) => {
                    cursor = None;
                    Some(Err(error.into()))
                }
            }
        })),
    ))
}

#[cfg(test)]
mod tests;
