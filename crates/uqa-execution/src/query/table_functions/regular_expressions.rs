//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning regular-expression functions drawn one row at a time from the SQL regular-expression owner.

use super::TableFunctionRows;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{
    expr::{RegexMatches, RegexSplit},
    SQLError,
};

/// Stream `regexp_matches(string, pattern [, flags])` as one `text[]` column under the call's input collation.
pub(super) fn regexp_matches_row_stream(
    evaluated: &[Value],
    collation: u32,
    column: String,
) -> Result<TableFunctionRows, SQLError> {
    let cursor = RegexMatches::new(evaluated, collation, &ProductionControl::uncontrolled())?;
    Ok(one_column_rows(cursor, column))
}

/// Stream `regexp_split_to_table(string, pattern [, flags])` as one `text` column under the call's input collation.
pub(super) fn regexp_split_to_table_row_stream(
    evaluated: &[Value],
    collation: u32,
    column: String,
) -> Result<TableFunctionRows, SQLError> {
    let cursor = RegexSplit::new(
        "regexp_split_to_table",
        evaluated,
        collation,
        &ProductionControl::uncontrolled(),
    )?;
    Ok(one_column_rows(cursor, column))
}

/// A SQL-owner cursor that produces one column value per row.
trait ValueCursor: Send + 'static {
    fn next_value(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>, SQLError>;
}

impl ValueCursor for RegexMatches {
    fn next_value(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>, SQLError> {
        self.next_row(control)
    }
}

impl ValueCursor for RegexSplit {
    fn next_value(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>, SQLError> {
        self.next_piece(control)
    }
}

/// Rows of one column drawn from `cursor` on demand, so one value is resident at a time; no cursor, for a NULL argument of a strict function, yields no rows. Allocation follows ordinary scalar evaluation, because `PostgreSQL`'s `work_mem` bounds sort and hash workspaces rather than function results, and the stream's caller checks cancellation before each row.
fn one_column_rows(mut cursor: Option<impl ValueCursor>, column: String) -> TableFunctionRows {
    TableFunctionRows::new(
        vec![column],
        crate::SQLRowStream::new(std::iter::from_fn(move || {
            let current = cursor.as_mut()?;
            match current.next_value(&ProductionControl::uncontrolled()) {
                Ok(Some(value)) => Some(Ok(crate::PhysicalRow::from_values(vec![
                    value.into_parts().0,
                ]))),
                Ok(None) => {
                    cursor = None;
                    None
                }
                Err(error) => {
                    cursor = None;
                    Some(Err(error))
                }
            }
        })),
    )
}

#[cfg(test)]
mod tests;
