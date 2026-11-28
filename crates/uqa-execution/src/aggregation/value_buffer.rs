//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spillable ordered aggregate-value buffering.

use super::ordered_buffer::{self, OrderedRecord, OrderedRows, OrderedSpillBuffer, RunReader};
use super::ordering::{compare_sort_keys, AggregateSortKey};
use super::{Ordering, SQLError, Value};
use std::io::{BufRead, Write};
use uqa_sql::expr::enums::EnumLabelCatalog;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct AggregateValueRecord {
    pub(super) value: Value,
    pub(super) sort_keys: Vec<super::ordering::AggregateSortKey>,
    pub(super) sequence: u64,
}

pub struct JsonSpillRun {
    pub(super) file: uqa_storage::temporary_file::TemporaryFile,
    pub(super) max_record_bytes: usize,
}

pub fn write_json_spill_record(
    writer: &mut impl Write,
    value: &impl serde::Serialize,
    description: &str,
) -> Result<usize, SQLError> {
    let payload = serde_json::to_vec(value).map_err(|error| {
        SQLError::Internal(format!("failed to serialize {description}: {error}"))
    })?;
    let record_bytes = payload
        .len()
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal(format!("{description} size overflow")))?;
    writer
        .write_all(&payload)
        .map_err(|error| SQLError::Internal(format!("failed to write {description}: {error}")))?;
    writer
        .write_all(b"\n")
        .map_err(|error| SQLError::Internal(format!("failed to write {description}: {error}")))?;
    Ok(record_bytes)
}

pub fn read_bounded_json_spill_record<R: BufRead>(
    reader: &mut R,
    max_record_bytes: usize,
    description: &str,
) -> Result<Option<Vec<u8>>, SQLError> {
    let mut record = Vec::new();
    loop {
        let (chunk_len, terminated) = {
            let available = reader.fill_buf().map_err(|error| {
                SQLError::Internal(format!("failed to read {description}: {error}"))
            })?;
            if available.is_empty() {
                if record.is_empty() {
                    return Ok(None);
                }
                return Err(SQLError::Internal(format!(
                    "truncated {description}: missing record delimiter"
                )));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (available.len(), false),
            }
        };
        let next_len = record
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| SQLError::Internal(format!("{description} length overflow")))?;
        if next_len > max_record_bytes {
            return Err(SQLError::Internal(format!(
                "{description} exceeds recorded maximum of {max_record_bytes} bytes"
            )));
        }
        record.try_reserve(chunk_len).map_err(|error| {
            SQLError::Internal(format!(
                "unable to allocate {chunk_len} more bytes for {description}: {error}"
            ))
        })?;
        let available = reader.fill_buf().map_err(|error| {
            SQLError::Internal(format!("failed to read {description}: {error}"))
        })?;
        record.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if terminated {
            let delimiter = record.pop();
            debug_assert_eq!(delimiter, Some(b'\n'));
            return Ok(Some(record));
        }
    }
}

pub type AggregateValueBuffer = OrderedSpillBuffer<AggregateValueRecord>;
pub type AggregateValueRunReader = RunReader<AggregateValueRecord>;

impl OrderedRecord for AggregateValueRecord {
    type Input = Value;

    fn new(value: Self::Input, sort_keys: Vec<AggregateSortKey>, sequence: u64) -> Self {
        Self {
            value,
            sort_keys,
            sequence,
        }
    }

    fn inputs(&self) -> &[Value] {
        std::slice::from_ref(&self.value)
    }

    fn input_payload_bytes(&self) -> usize {
        ordered_buffer::payload_bytes(&self.value)
    }

    fn keys(&self) -> &Vec<AggregateSortKey> {
        &self.sort_keys
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn read_input(
        reader: &mut crate::spill::BinaryReader<'_>,
        count: usize,
    ) -> crate::ExecResult<Self::Input> {
        if count != 1 {
            return Err(crate::ExecError::Other(
                "aggregate spill row must contain one value".into(),
            ));
        }
        reader.read_value(0)
    }
}

impl OrderedRows<'_, AggregateValueRecord> {
    pub(super) fn ordered_values(
        self,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<Vec<Value>, SQLError> {
        let capacity = usize::try_from(self.buffer.next_sequence).map_err(|_| {
            SQLError::Internal("aggregate value count exceeds address space".into())
        })?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity).map_err(|error| {
            SQLError::Internal(format!(
                "unable to allocate aggregate result for {capacity} values: {error}"
            ))
        })?;
        self.for_each_ordered(enums, |record| {
            values.push(record.value);
            Ok(())
        })?;
        Ok(values)
    }
}

pub fn read_aggregate_value_record(
    reader: &mut impl std::io::BufRead,
    max_record_bytes: usize,
) -> Result<Option<AggregateValueRecord>, SQLError> {
    let Some(record) =
        read_bounded_json_spill_record(reader, max_record_bytes, "aggregate spill row")?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&record).map(Some).map_err(|error| {
        SQLError::Internal(format!(
            "failed to deserialize aggregate spill row: {error}"
        ))
    })
}

pub fn merge_aggregate_value_runs(runs: Vec<JsonSpillRun>) -> Result<JsonSpillRun, SQLError> {
    ordered_buffer::merge_json::<AggregateValueRecord>(&runs)
}

pub fn compare_aggregate_value_records(
    left: &AggregateValueRecord,
    right: &AggregateValueRecord,
) -> Result<Ordering, SQLError> {
    Ok(compare_sort_keys(left.keys(), right.keys())?
        .then_with(|| left.sequence.cmp(&right.sequence)))
}
