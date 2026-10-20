//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed, positional view-rule inputs retain each value's original assignment type.

use uqa_core::{CancellationToken, Value};
use uqa_sql::{ast::ColumnType, SQLError};

use crate::physical::physical_exec_error;
use crate::query::output::{PublicQueryRows, QueryOutput};
use crate::spill::BufferedIndexedSpill;
use crate::{PhysicalRow, RowSchema};

type TypedInput = Vec<(Value, Option<ColumnType>)>;

pub(super) struct ViewInputBuffer {
    rows: BufferedIndexedSpill,
    cancellation: CancellationToken,
}

impl ViewInputBuffer {
    pub(super) fn new(bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            rows: BufferedIndexedSpill::new(RowSchema::new(vec![String::new(); 2]), bytes),
            cancellation,
        }
    }

    pub(super) fn push(&mut self, input: TypedInput) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let (values, types): (Vec<_>, Vec<_>) = input.into_iter().unzip();
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&types, &mut encoded)
            .map_err(|error| invalid(format!("cannot encode source types: {error}")))?;
        self.rows
            .push(&PhysicalRow::from_values(vec![
                Value::List(values),
                Value::Bytes(encoded),
            ]))
            .map_err(physical_exec_error)
    }

    pub(super) fn finish(self) -> ViewInputRows {
        ViewInputRows {
            rows: ViewInputStorage::Values {
                rows: self.rows,
                position: 0,
            },
            cancellation: self.cancellation,
        }
    }
}

enum ViewInputStorage {
    Values {
        rows: BufferedIndexedSpill,
        position: u64,
    },
    Query {
        types: Vec<Option<ColumnType>>,
        rows: Box<PublicQueryRows>,
    },
}

pub(super) struct ViewInputRows {
    rows: ViewInputStorage,
    cancellation: CancellationToken,
}

impl ViewInputRows {
    pub(super) fn query(
        output: QueryOutput,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        let types = output.column_types.clone();
        let rows = PublicQueryRows::new(output, "view INSERT rule input")?;
        Ok(Self {
            rows: ViewInputStorage::Query {
                types,
                rows: Box::new(rows),
            },
            cancellation,
        })
    }
}

impl Iterator for ViewInputRows {
    type Item = Result<TypedInput, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Err(error) = self.cancellation.check() {
            return Some(Err(error.into()));
        }
        match &mut self.rows {
            ViewInputStorage::Values { rows, position } => {
                if *position == rows.len() {
                    return None;
                }
                let row = rows.get(*position).map_err(physical_exec_error);
                *position += 1;
                Some(row.and_then(decode))
            }
            ViewInputStorage::Query { types, rows } => rows.next().map(|row| {
                row.map(|values| {
                    values
                        .into_iter()
                        .enumerate()
                        .map(|(position, value)| (value, types.get(position).cloned().flatten()))
                        .collect()
                })
            }),
        }
    }
}

fn decode(row: PhysicalRow) -> Result<TypedInput, SQLError> {
    let [Value::List(values), Value::Bytes(encoded)] =
        <[Value; 2]>::try_from(row.into_physical_values())
            .map_err(|_| invalid("invalid encoded row width"))?
    else {
        return Err(invalid("invalid encoded values or types"));
    };
    let mut input = std::io::Cursor::new(encoded.as_slice());
    let types: Vec<Option<ColumnType>> = ciborium::de::from_reader(&mut input)
        .map_err(|error| invalid(format!("cannot decode source types: {error}")))?;
    if input.position() != encoded.len() as u64 || types.len() != values.len() {
        return Err(invalid("source type metadata does not match its values"));
    }
    Ok(values.into_iter().zip(types).collect())
}

fn invalid(message: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("view INSERT input: {message}"))
}

#[cfg(test)]
mod tests;
