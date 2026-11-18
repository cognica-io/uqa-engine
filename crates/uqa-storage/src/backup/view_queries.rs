//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog text projections share the physical export cut and retire each selected row independently.

use super::{
    BackupError, BackupResult, BackupSourceStatistics, RecordImageCursor, RecordImageStep,
};
use crate::{
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
};
use std::ops::Range;
use uqa_core::{
    image_io::ImageUtf8,
    memory::{Budgeted, BudgetedString, BudgetedVec},
};

mod fields;
use fields::{FieldProjector, FieldStep, StringBytes};

const READ_WINDOW: usize = 1024;
type TextRange = fn(&mut RecordImageCursor) -> BackupResult<Range<usize>>;

#[derive(Clone, Copy)]
enum Envelope {
    Text(TextRange),
    KeyValue,
}

#[derive(Clone, Copy)]
enum Projection {
    View(Envelope),
    Metadata(Option<TextRange>),
}

type KeySelection = fn(&[u8], &str, &StorageReadControl) -> crate::StorageBackendResult<bool>;

struct Filter {
    prefix: BudgetedString,
    select: KeySelection,
}

/// Reads projected view queries or metadata values from the original committed image records. The key limit applies before retaining a physical identity and the value limit applies to the returned text.
pub struct BackupCatalogTextInput {
    input: Option<Budgeted<Box<Input>>>,
    statistics: BackupSourceStatistics,
    failed: bool,
}

struct Input {
    records: RecordImageCursor,
    source_cancellation: uqa_core::CancellationToken,
    projection: Projection,
    filter: Option<Filter>,
    limits: CatalogInputLimits,
    control: StorageReadControl,
    row: Option<Row>,
}

impl BackupCatalogTextInput {
    /// The native mapping supplies the byte range of the original UTF-8 definition in its record.
    pub fn from_text_records(
        records: &RecordImageCursor,
        prefix: &[u8],
        locate: TextRange,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::new(
            records,
            prefix,
            Projection::View(Envelope::Text(locate)),
            None,
            limits,
            control,
        )
    }

    pub(crate) fn from_key_value_records(
        records: &RecordImageCursor,
        prefix: &[u8],
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::new(
            records,
            prefix,
            Projection::View(Envelope::KeyValue),
            None,
            limits,
            control,
        )
    }

    /// Select native text values through their existing ordered physical prefix and catalog row layout.
    pub fn from_metadata_records(
        records: &RecordImageCursor,
        prefix: &[u8],
        locate: TextRange,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::new(
            records,
            prefix,
            Projection::Metadata(Some(locate)),
            None,
            limits,
            control,
        )
    }

    pub(crate) fn from_filtered_metadata_records(
        records: &RecordImageCursor,
        physical_prefix: &[u8],
        logical_prefix: &str,
        select: KeySelection,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check_value_size(logical_prefix.len(), limits.maximum_key_bytes)?;
        let mut prefix = BudgetedString::new(control.memory());
        prefix.push_str(logical_prefix)?;
        Self::new(
            records,
            physical_prefix,
            Projection::Metadata(None),
            Some(Filter { prefix, select }),
            limits,
            control,
        )
    }

    fn new(
        records: &RecordImageCursor,
        prefix: &[u8],
        projection: Projection,
        filter: Option<Filter>,
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
                    projection,
                    filter,
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
        self.statistics.metadata_read_bytes = input
            .records
            .copied_bytes()
            .saturating_add(input.records.fingerprint_bytes());
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
                    *row = Row::new(
                        row.range.clone(),
                        self.projection,
                        true,
                        self.limits.maximum_value_bytes,
                        &self.control,
                    )?;
                    return Ok(CatalogInputStep::Pending);
                }
                Next::Done => (),
            }
            let output = self
                .row
                .take()
                .expect("completed catalog text")
                .into_bytes();
            self.records.skip_current()?;
            return Ok(CatalogInputStep::Row(output));
        }
        match self.records.advance(1)? {
            RecordImageStep::Pending => Ok(CatalogInputStep::Pending),
            RecordImageStep::Complete => Ok(CatalogInputStep::Complete),
            RecordImageStep::Unit => {
                let unit = self.records.current_unit().expect("selected record");
                if let Some(filter) = &self.filter {
                    if !(filter.select)(unit.key, &filter.prefix, &self.control)? {
                        self.records.skip_current()?;
                        return Ok(CatalogInputStep::Pending);
                    }
                }
                let length = unit.decoded_bytes;
                let range = match self.projection {
                    Projection::View(Envelope::Text(locate))
                    | Projection::Metadata(Some(locate)) => locate(&mut self.records)?,
                    Projection::View(Envelope::KeyValue) | Projection::Metadata(None) => 0..length,
                };
                if range.start > range.end || range.end > length {
                    return Err(BackupError::Invalid("catalog text exceeds its record"));
                }
                self.row = Some(Row::new(
                    range,
                    self.projection,
                    false,
                    self.limits.maximum_value_bytes,
                    &self.control,
                )?);
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
    view: View,
    output: Output,
}

enum Output {
    Query(BudgetedVec<u8>),
    Metadata {
        text: BudgetedString,
        utf8: ImageUtf8,
    },
}

#[derive(Default)]
struct View {
    wrapper: Option<FieldProjector>,
    string: StringBytes,
    query: Option<FieldProjector>,
}

impl Row {
    fn new(
        range: Range<usize>,
        projection: Projection,
        legacy: bool,
        maximum: usize,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let (view, output) = match projection {
            Projection::View(envelope) => (
                View {
                    wrapper: matches!(envelope, Envelope::KeyValue)
                        .then(FieldProjector::definition),
                    string: StringBytes::default(),
                    query: (!legacy).then(FieldProjector::query),
                },
                Output::Query(BudgetedVec::new(control.memory())),
            ),
            Projection::Metadata(_) => {
                control.check_value_size(range.len(), maximum)?;
                let mut text = BudgetedString::new(control.memory());
                text.reserve(range.len())?;
                (
                    View::default(),
                    Output::Metadata {
                        text,
                        utf8: ImageUtf8::default(),
                    },
                )
            }
        };
        Ok(Self {
            offset: range.start,
            range,
            view,
            output,
        })
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        control: &StorageReadControl,
        maximum: usize,
    ) -> BackupResult<Next> {
        let end = self.offset == self.range.end;
        match &mut self.output {
            Output::Query(output) => self.view.feed(bytes, end, control, maximum, output),
            Output::Metadata { text, utf8 } => {
                utf8.append(bytes, text)
                    .map_err(crate::StorageBackendError::from)?;
                if end {
                    utf8.finish().map_err(crate::StorageBackendError::from)?;
                    Ok(Next::Done)
                } else {
                    Ok(Next::More)
                }
            }
        }
    }

    fn into_bytes(self) -> Budgeted<Vec<u8>> {
        let (bytes, memory) = match self.output {
            Output::Query(output) => output.into_parts(),
            Output::Metadata { text, .. } => {
                let (text, memory) = text.into_parts();
                (text.into_bytes(), memory)
            }
        };
        Budgeted::new(bytes, memory)
    }
}

impl View {
    fn feed(
        &mut self,
        bytes: &[u8],
        end: bool,
        control: &StorageReadControl,
        maximum: usize,
        output: &mut BudgetedVec<u8>,
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
                    match self.query_byte(inner, control, maximum, output)? {
                        Next::More => (),
                        result => return Ok(result),
                    }
                }
            } else {
                match self.query_byte(byte, control, maximum, output)? {
                    Next::More => (),
                    result => return Ok(result),
                }
            }
        }
        if end {
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
        output: &mut BudgetedVec<u8>,
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
        if output.len() >= maximum {
            control.check_value_size(output.len().saturating_add(1), maximum)?;
        }
        output.push(byte)?;
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
