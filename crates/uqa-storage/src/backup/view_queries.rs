//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored query projections share the physical export cut and retire each selected row independently.

use super::{
    BackupError, BackupResult, BackupSourceStatistics, RecordImageCursor, RecordImageStep,
};
use crate::{
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
};
use std::ops::Range;
use uqa_core::memory::{Budgeted, BudgetedVec};

mod fields;
use fields::{FieldProjector, FieldStep, StringBytes};

const READ_WINDOW: usize = 1024;
type TextRange = fn(&mut RecordImageCursor) -> BackupResult<Range<usize>>;

#[derive(Clone, Copy)]
enum Envelope {
    Text(TextRange),
    KeyValue,
}

/// Reads only the query in each stored view, using the image's original committed record revisions. The key limit applies before retaining a physical identity and the value limit applies to the projected query. Evaluated rows and security metadata never become query output. Native and Key/Value mappings supply their own physical envelopes.
pub struct BackupViewQueryInput {
    input: Option<Budgeted<Box<Input>>>,
    statistics: BackupSourceStatistics,
    failed: bool,
}

struct Input {
    records: RecordImageCursor,
    source_cancellation: uqa_core::CancellationToken,
    envelope: Envelope,
    limits: CatalogInputLimits,
    control: StorageReadControl,
    row: Option<Row>,
}

impl BackupViewQueryInput {
    /// The native mapping supplies the byte range of the original UTF-8 definition in its record.
    pub fn from_text_records(
        records: &RecordImageCursor,
        prefix: &[u8],
        locate: TextRange,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::new(records, prefix, Envelope::Text(locate), limits, control)
    }

    pub(crate) fn from_key_value_records(
        records: &RecordImageCursor,
        prefix: &[u8],
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::new(records, prefix, Envelope::KeyValue, limits, control)
    }

    fn new(
        records: &RecordImageCursor,
        prefix: &[u8],
        envelope: Envelope,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        limits.validate()?;
        let memory = control.memory().reserve(size_of::<Input>())?;
        let source_cancellation = records.cancellation().clone();
        let records = records.record_prefix(prefix, limits.maximum_key_bytes, control)?;
        Ok(Self {
            input: Some(Budgeted::new(
                Box::new(Input {
                    records,
                    source_cancellation,
                    envelope,
                    limits,
                    control: control.clone(),
                    row: None,
                }),
                memory,
            )),
            statistics: BackupSourceStatistics::default(),
            failed: false,
        })
    }

    /// Visit one physical key or read at most one payload window. An error retires the independent catalog cursor and its partial output.
    pub fn advance(&mut self) -> BackupResult<CatalogInputStep<Vec<u8>>> {
        let Some(input) = self.input.as_mut() else {
            return if self.failed {
                Err(BackupError::Finished)
            } else {
                Ok(CatalogInputStep::Complete)
            };
        };
        let result = input.as_mut().advance();
        self.statistics.visited_positions = input.records.visited_positions();
        self.statistics.metadata_read_bytes = input.records.copied_bytes();
        if matches!(result, Ok(CatalogInputStep::Complete) | Err(_)) {
            self.failed = result.is_err();
            self.input = None;
        }
        result
    }

    pub const fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
}

impl Input {
    fn advance(&mut self) -> BackupResult<CatalogInputStep<Vec<u8>>> {
        self.source_cancellation.check()?;
        self.control.check()?;
        if let Some(row) = &mut self.row {
            let mut bytes = [0; READ_WINDOW];
            let count = (row.range.end - row.offset).min(bytes.len());
            self.records
                .read_record_at(row.offset, &mut bytes[..count])?;
            row.offset += count;
            match row.feed(
                &bytes[..count],
                &self.control,
                self.limits.maximum_value_bytes,
            )? {
                Next::More => return Ok(CatalogInputStep::Pending),
                Next::Legacy => {
                    *row = Row::new(row.range.clone(), self.envelope, true, &self.control);
                    return Ok(CatalogInputStep::Pending);
                }
                Next::Done => (),
            }
            let output = self.row.take().expect("completed query").output;
            self.records.skip_current()?;
            let (bytes, memory) = output.into_parts();
            return Ok(CatalogInputStep::Row(Budgeted::new(bytes, memory)));
        }
        match self.records.advance(1)? {
            RecordImageStep::Pending => Ok(CatalogInputStep::Pending),
            RecordImageStep::Complete => Ok(CatalogInputStep::Complete),
            RecordImageStep::Unit => {
                let length = self
                    .records
                    .current_unit()
                    .expect("selected record")
                    .decoded_bytes;
                let range = match self.envelope {
                    Envelope::Text(locate) => locate(&mut self.records)?,
                    Envelope::KeyValue => 0..length,
                };
                if range.start > range.end || range.end > length {
                    return Err(BackupError::Invalid(
                        "stored view definition exceeds its record",
                    ));
                }
                self.row = Some(Row::new(range, self.envelope, false, &self.control));
                Ok(CatalogInputStep::Pending)
            }
        }
    }
}

enum Next {
    More,
    Legacy,
    Done,
}

struct Row {
    range: Range<usize>,
    offset: usize,
    wrapper: Option<FieldProjector>,
    string: StringBytes,
    query: Option<FieldProjector>,
    output: BudgetedVec<u8>,
}

impl Row {
    fn new(
        range: Range<usize>,
        envelope: Envelope,
        legacy: bool,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            offset: range.start,
            range,
            wrapper: matches!(envelope, Envelope::KeyValue).then(FieldProjector::definition),
            string: StringBytes::default(),
            query: (!legacy).then(FieldProjector::query),
            output: BudgetedVec::new(control.memory()),
        }
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        control: &StorageReadControl,
        maximum: usize,
    ) -> BackupResult<Next> {
        for &byte in bytes {
            if let Some(wrapper) = self.wrapper.as_mut() {
                match wrapper.feed(byte, control)? {
                    FieldStep::Ignore => continue,
                    FieldStep::Absent => return Err(invalid()),
                    FieldStep::Last => {
                        self.string.finish()?;
                        return if self.query.is_none() {
                            Ok(Next::Done)
                        } else {
                            Err(invalid())
                        };
                    }
                    FieldStep::Selected => (),
                }
                let decoded = self.string.feed(byte, control)?;
                for &inner in decoded.as_slice() {
                    match self.query_byte(inner, control, maximum)? {
                        Next::More => (),
                        result => return Ok(result),
                    }
                }
            } else {
                match self.query_byte(byte, control, maximum)? {
                    Next::More => (),
                    result => return Ok(result),
                }
            }
        }
        if self.offset == self.range.end {
            if self.wrapper.is_none() && self.query.is_none() {
                return Ok(Next::Done);
            }
            return Err(invalid());
        }
        Ok(Next::More)
    }

    fn query_byte(
        &mut self,
        byte: u8,
        control: &StorageReadControl,
        maximum: usize,
    ) -> BackupResult<Next> {
        let step = if let Some(query) = self.query.as_mut() {
            match query.feed(byte, control)? {
                FieldStep::Ignore => return Ok(Next::More),
                FieldStep::Absent if query.is_legacy() => return Ok(Next::Legacy),
                FieldStep::Absent => return Err(invalid()),
                selected => selected,
            }
        } else {
            FieldStep::Selected
        };
        if self.output.len() >= maximum {
            control.check_value_size(self.output.len().saturating_add(1), maximum)?;
        }
        self.output.push(byte)?;
        Ok(if matches!(step, FieldStep::Last) {
            Next::Done
        } else {
            Next::More
        })
    }
}

fn invalid() -> BackupError {
    BackupError::Invalid("stored view query projection")
}

#[cfg(test)]
mod tests;
