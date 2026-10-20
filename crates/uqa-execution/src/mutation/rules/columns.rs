//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed required-column metadata retains compact equal runs or bounded spill.

use std::collections::BTreeSet;

use uqa_core::{CancellationToken, Value};
use uqa_sql::SQLError;

use crate::mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id};
use crate::physical::physical_exec_error;
use crate::spill::{SharedSpill, SharedSpillReader, SpillBatchWriter, SpillBuffer, SpillRows};
use crate::{PhysicalRow, RowSchema};

mod selection;

type Columns<const N: usize> = [BTreeSet<String>; N];

/// Complete projection requirements, computed before supplemental SQL evaluation starts.
pub struct RuleColumnSets<const N: usize> {
    storage: Storage<N>,
    count: usize,
    any: bool,
    cancellation: CancellationToken,
}

enum Storage<const N: usize> {
    Uniform(Columns<N>),
    Spill(SharedSpill),
}

impl<const N: usize> RuleColumnSets<N> {
    pub(super) fn from_rows(
        input: impl IntoIterator<Item = Result<Columns<N>, SQLError>>,
        bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut rows = SpillBuffer::new(bytes);
        let mut writer = SpillBatchWriter::new(schema::<N>()).map_err(physical_exec_error)?;
        let mut pending = None::<(usize, Columns<N>)>;
        let mut count = 0_usize;
        let mut any = false;
        let mut flushed = false;
        for columns in input {
            cancellation.check()?;
            let columns = columns?;
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("row count overflow"))?;
            any |= columns.iter().any(|columns| !columns.is_empty());
            if let Some((length, previous)) = &mut pending {
                if *previous == columns {
                    *length += 1;
                    continue;
                }
            }
            if let Some(run) = pending.take() {
                writer
                    .push(&mut rows, encode(run))
                    .map_err(physical_exec_error)?;
                flushed = true;
            }
            pending = Some((1, columns));
        }
        cancellation.check()?;
        let storage = if flushed {
            if let Some(run) = pending {
                writer
                    .push(&mut rows, encode(run))
                    .map_err(physical_exec_error)?;
            }
            writer.finish(&mut rows).map_err(physical_exec_error)?;
            Storage::Spill(
                rows.into_shared(schema::<N>())
                    .map_err(physical_exec_error)?,
            )
        } else {
            Storage::Uniform(pending.map_or_else(empty, |(_, columns)| columns))
        };
        Ok(Self {
            storage,
            count,
            any,
            cancellation,
        })
    }

    pub fn has_required_columns(&self) -> bool {
        self.any
    }

    pub fn reader(&self) -> Result<RuleColumnIter<N>, SQLError> {
        self.cancellation.check()?;
        let (pending, rows) = match &self.storage {
            Storage::Uniform(columns) => ((self.count, columns.clone()), None),
            Storage::Spill(rows) => (
                (0, empty()),
                Some(Box::new(rows.read_rows().map_err(physical_exec_error)?)),
            ),
        };
        Ok(RuleColumnIter {
            pending,
            rows,
            remaining: self.count,
            cancellation: self.cancellation.clone(),
            finished: false,
        })
    }
}

/// Owned replay remains valid when a batch replaces its original event images.
pub struct RuleColumnIter<const N: usize> {
    pending: (usize, Columns<N>),
    rows: Option<Box<SpillRows<SharedSpillReader>>>,
    remaining: usize,
    cancellation: CancellationToken,
    finished: bool,
}

impl<const N: usize> RuleColumnIter<N> {
    fn read_next(&mut self) -> Result<Option<Columns<N>>, SQLError> {
        self.cancellation.check()?;
        if self.pending.0 == 0 {
            if let Some(row) = self.rows.as_mut().and_then(Iterator::next) {
                self.pending = decode(row.map_err(physical_exec_error)?.row)?;
                if self.pending.0 > self.remaining {
                    return Err(invalid("run exceeds the declared row count"));
                }
            }
        }
        if self.pending.0 == 0 {
            return if self.remaining == 0 {
                Ok(None)
            } else {
                Err(invalid("missing required-column rows"))
            };
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| invalid("more rows than declared"))?;
        self.pending.0 -= 1;
        Ok(Some(self.pending.1.clone()))
    }
}

impl<const N: usize> Iterator for RuleColumnIter<N> {
    type Item = Result<Columns<N>, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.read_next() {
            Ok(Some(columns)) => Some(Ok(columns)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

fn empty<const N: usize>() -> Columns<N> {
    std::array::from_fn(|_| BTreeSet::new())
}

fn schema<const N: usize>() -> RowSchema {
    RowSchema::new(
        std::iter::once("count".into())
            .chain((0..N).map(|side| format!("columns_{side}")))
            .collect(),
    )
}

fn encode<const N: usize>((count, columns): (usize, Columns<N>)) -> PhysicalRow {
    PhysicalRow::from_values(
        std::iter::once(encode_prepared_doc_id(count as u64))
            .chain(
                columns
                    .into_iter()
                    .map(|columns| Value::List(columns.into_iter().map(Value::Str).collect())),
            )
            .collect(),
    )
}

fn decode<const N: usize>(row: PhysicalRow) -> Result<(usize, Columns<N>), SQLError> {
    let mut values = row.into_physical_values().into_iter();
    let count = usize::try_from(decode_prepared_doc_id(
        values.next().ok_or_else(|| invalid("missing run length"))?,
        "rewrite-rule required columns",
    )?)
    .map_err(|_| invalid("run exceeds the addressable row count"))?;
    if count == 0 {
        return Err(invalid("empty run"));
    }
    let mut output = empty();
    for columns in &mut output {
        let Some(Value::List(names)) = values.next() else {
            return Err(invalid("invalid column set"));
        };
        for name in names {
            let Value::Str(name) = name else {
                return Err(invalid("column name is not text"));
            };
            if !columns.insert(name) {
                return Err(invalid("duplicate column name"));
            }
        }
    }
    if values.next().is_some() {
        return Err(invalid("invalid record width"));
    }
    Ok((count, output))
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("rewrite-rule required columns: {message}"))
}

#[cfg(test)]
mod tests;
