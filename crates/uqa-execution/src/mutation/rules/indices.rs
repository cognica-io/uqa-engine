//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered event ordinals coalesce adjacent matches and spill fragmented selections.

mod lookup;

use std::ops::Range;

use uqa_core::CancellationToken;
use uqa_sql::SQLError;

use crate::mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id};
use crate::physical::physical_exec_error;
use crate::spill::{SharedSpill, SharedSpillReader, SpillBatchWriter, SpillBuffer, SpillRows};
use crate::{PhysicalRow, RowSchema};

pub(crate) struct RuleIndices {
    storage: IndexStorage,
    count: usize,
    cancellation: CancellationToken,
}

enum IndexStorage {
    Range(Range<usize>),
    Spill(SharedSpill),
}

impl RuleIndices {
    pub(crate) fn all(count: usize, cancellation: CancellationToken) -> Self {
        Self {
            storage: IndexStorage::Range(0..count),
            count,
            cancellation,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn is_all(&self, count: usize) -> bool {
        self.count == count
            && matches!(&self.storage, IndexStorage::Range(range) if *range == (0..count))
    }

    pub(crate) fn reader(&self) -> Result<RuleIndexIter, SQLError> {
        self.cancellation.check()?;
        let (pending, rows) = match &self.storage {
            IndexStorage::Range(range) => (range.clone(), None),
            IndexStorage::Spill(rows) => (
                0..0,
                Some(Box::new(rows.read_rows().map_err(physical_exec_error)?)),
            ),
        };
        Ok(RuleIndexIter {
            pending,
            rows,
            remaining: self.count,
            cancellation: self.cancellation.clone(),
            finished: false,
        })
    }
}

pub(crate) struct RuleIndexBuffer {
    rows: SpillBuffer,
    writer: SpillBatchWriter,
    pending: Option<Range<usize>>,
    count: usize,
    flushed: bool,
    cancellation: CancellationToken,
}

impl RuleIndexBuffer {
    pub(crate) fn new(bytes: usize, cancellation: CancellationToken) -> Result<Self, SQLError> {
        Ok(Self {
            rows: SpillBuffer::new(bytes),
            writer: SpillBatchWriter::new(schema()).map_err(physical_exec_error)?,
            pending: None,
            count: 0,
            flushed: false,
            cancellation,
        })
    }

    pub(crate) fn push(&mut self, index: usize) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let end = index
            .checked_add(1)
            .ok_or_else(|| invalid("event ordinal exceeds the addressable row count"))?;
        let count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("selection exceeds the addressable row count"))?;
        if let Some(pending) = &mut self.pending {
            if pending.end == index {
                pending.end = end;
                self.count = count;
                return Ok(());
            }
            self.flush_pending()?;
        }
        self.pending = Some(index..end);
        self.count = count;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<RuleIndices, SQLError> {
        self.cancellation.check()?;
        let storage = if self.flushed {
            self.flush_pending()?;
            self.writer
                .finish(&mut self.rows)
                .map_err(physical_exec_error)?;
            IndexStorage::Spill(
                self.rows
                    .into_shared(schema())
                    .map_err(physical_exec_error)?,
            )
        } else {
            IndexStorage::Range(self.pending.unwrap_or(0..0))
        };
        Ok(RuleIndices {
            storage,
            count: self.count,
            cancellation: self.cancellation,
        })
    }

    fn flush_pending(&mut self) -> Result<(), SQLError> {
        if let Some(range) = self.pending.take() {
            self.writer
                .push(
                    &mut self.rows,
                    PhysicalRow::from_values(vec![
                        encode_prepared_doc_id(range.start as u64),
                        encode_prepared_doc_id(range.end as u64),
                    ]),
                )
                .map_err(physical_exec_error)?;
            self.flushed = true;
        }
        Ok(())
    }
}

pub(crate) struct RuleIndexIter {
    pending: Range<usize>,
    rows: Option<Box<SpillRows<SharedSpillReader>>>,
    remaining: usize,
    cancellation: CancellationToken,
    finished: bool,
}

impl RuleIndexIter {
    fn read_next(&mut self) -> Result<Option<usize>, SQLError> {
        self.cancellation.check()?;
        loop {
            if let Some(index) = self.pending.next() {
                self.remaining = self
                    .remaining
                    .checked_sub(1)
                    .ok_or_else(|| invalid("selection contains more ordinals than declared"))?;
                return Ok(Some(index));
            }
            let Some(row) = self.rows.as_mut().and_then(Iterator::next) else {
                return if self.remaining == 0 {
                    Ok(None)
                } else {
                    Err(invalid("selection omitted declared ordinals"))
                };
            };
            let range = decode_range(row.map_err(physical_exec_error)?.row)?;
            if range.len() > self.remaining {
                return Err(invalid("selection contains more ordinals than declared"));
            }
            self.pending = range;
        }
    }
}

impl Iterator for RuleIndexIter {
    type Item = Result<usize, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.read_next() {
            Ok(Some(index)) => Some(Ok(index)),
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

fn decode_range(row: PhysicalRow) -> Result<Range<usize>, SQLError> {
    let [start, end] = <[_; 2]>::try_from(row.into_physical_values())
        .map_err(|_| invalid("invalid range record width"))?;
    let decode = |value| {
        usize::try_from(decode_prepared_doc_id(value, "rewrite-rule selection")?)
            .map_err(|_| invalid("ordinal exceeds the addressable row count"))
    };
    let range = decode(start)?..decode(end)?;
    if range.is_empty() {
        return Err(invalid("invalid empty or reversed range"));
    }
    Ok(range)
}

fn schema() -> RowSchema {
    RowSchema::new(vec!["start".into(), "end".into()])
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("rewrite-rule selection: {message}"))
}

#[cfg(test)]
mod tests;
