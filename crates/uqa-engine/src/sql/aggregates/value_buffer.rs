//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spillable ordered aggregate-value buffering.

use super::{
    BufRead, BufReader, BufWriter, File, Ordering, SQLError, Seek, SeekFrom, Value, Write,
    AGGREGATE_MERGE_FAN_IN,
};

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(in crate::sql) struct AggregateValueRecord {
    pub(super) value: Value,
    pub(super) sort_keys: Vec<(Value, bool)>,
    pub(super) sequence: u64,
}

pub(in crate::sql) struct JsonSpillRun {
    pub(super) file: tempfile::NamedTempFile,
    pub(super) max_record_bytes: usize,
}

pub(in crate::sql) fn write_json_spill_record(
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

pub(in crate::sql) fn read_bounded_json_spill_record<R: BufRead>(
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

pub(in crate::sql) struct AggregateValueBuffer {
    pub(super) rows: Vec<AggregateValueRecord>,
    pub(super) runs: Vec<JsonSpillRun>,
    pub(super) next_sequence: u64,
    pub(super) budget_bytes: usize,
    pub(super) memory_bytes: usize,
}

impl Default for AggregateValueBuffer {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl AggregateValueBuffer {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            budget_bytes: budget_bytes.max(1),
            memory_bytes: 0,
        }
    }

    pub(super) fn push(
        &mut self,
        value: Value,
        sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate value sequence overflow".into()))?;
        let record = AggregateValueRecord {
            value,
            sort_keys,
            sequence: self.next_sequence,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| {
                SQLError::Internal(format!("failed to size aggregate value: {error}"))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate value size overflow".into()))?;
        if !self.rows.is_empty()
            && self
                .memory_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > self.budget_bytes)
        {
            self.flush_run()?;
        }
        let next_memory_bytes = self
            .memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| SQLError::Internal("aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.memory_bytes = next_memory_bytes;
        self.next_sequence = next_sequence;
        // One value is indivisible. It passes through memory once and is
        // immediately written when its encoding alone exceeds the budget.
        if self.memory_bytes > self.budget_bytes {
            self.flush_run()?;
        }
        Ok(())
    }

    pub(super) fn ordered_values(&self) -> Result<Vec<Value>, SQLError> {
        let capacity = usize::try_from(self.next_sequence).map_err(|_| {
            SQLError::Internal("aggregate value count exceeds address space".into())
        })?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity).map_err(|error| {
            SQLError::Internal(format!(
                "unable to allocate aggregate result for {capacity} values: {error}"
            ))
        })?;
        self.for_each_ordered(|record| {
            values.push(record.value);
            Ok(())
        })?;
        Ok(values)
    }

    pub(super) fn for_each_ordered(
        &self,
        mut visit: impl FnMut(AggregateValueRecord) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let mut memory = self.rows.clone();
        memory.sort_by(compare_aggregate_value_records);
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!memory.is_empty()));
        if !memory.is_empty() {
            readers.push(AggregateValueRunReader::memory(memory));
        }
        for run in &self.runs {
            readers.push(AggregateValueRunReader::file(run)?);
        }
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_aggregate_value_records(left, right))
        {
            visit(readers[index].take_current()?)?;
        }
        Ok(())
    }

    pub(super) fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(compare_aggregate_value_records);
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!("failed to create aggregate spill file: {err}"))
        })?;
        let mut max_record_bytes = 0;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                let record_bytes =
                    write_json_spill_record(&mut writer, &row, "aggregate spill row")?;
                max_record_bytes = max_record_bytes.max(record_bytes);
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!("failed to flush aggregate spill file: {err}"))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!("failed to rewind aggregate spill file: {err}"))
        })?;
        self.runs.push(JsonSpillRun {
            file: run,
            max_record_bytes,
        });
        self.memory_bytes = 0;
        if self.runs.len() >= AGGREGATE_MERGE_FAN_IN {
            let inputs = self
                .runs
                .drain(..AGGREGATE_MERGE_FAN_IN)
                .collect::<Vec<_>>();
            self.runs.push(merge_aggregate_value_runs(inputs)?);
        }
        Ok(())
    }
}

pub(in crate::sql) enum AggregateValueRunReader {
    Memory {
        rows: std::vec::IntoIter<AggregateValueRecord>,
        current: Option<AggregateValueRecord>,
    },
    File {
        reader: BufReader<File>,
        current: Option<AggregateValueRecord>,
        max_record_bytes: usize,
    },
}

impl AggregateValueRunReader {
    pub(super) fn memory(rows: Vec<AggregateValueRecord>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    pub(super) fn file(run: &JsonSpillRun) -> Result<Self, SQLError> {
        let file = run.file.reopen().map_err(|error| {
            SQLError::Internal(format!("failed to reopen aggregate spill file: {error}"))
        })?;
        let mut reader = BufReader::new(file);
        let current = read_aggregate_value_record(&mut reader, run.max_record_bytes)?;
        Ok(Self::File {
            reader,
            current,
            max_record_bytes: run.max_record_bytes,
        })
    }

    pub(super) fn current(&self) -> Option<&AggregateValueRecord> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    pub(super) fn take_current(&mut self) -> Result<AggregateValueRecord, SQLError> {
        match self {
            Self::Memory { rows, current } => {
                let record = current
                    .take()
                    .ok_or_else(|| SQLError::Internal("aggregate memory run exhausted".into()))?;
                *current = rows.next();
                Ok(record)
            }
            Self::File {
                reader,
                current,
                max_record_bytes,
            } => {
                let record = current
                    .take()
                    .ok_or_else(|| SQLError::Internal("aggregate spill run exhausted".into()))?;
                *current = read_aggregate_value_record(reader, *max_record_bytes)?;
                Ok(record)
            }
        }
    }
}

pub(in crate::sql) fn read_aggregate_value_record(
    reader: &mut BufReader<File>,
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

pub(in crate::sql) fn merge_aggregate_value_runs(
    runs: Vec<JsonSpillRun>,
) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(AggregateValueRunReader::file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = tempfile::NamedTempFile::new().map_err(|error| {
        SQLError::Internal(format!("failed to create aggregate merge run: {error}"))
    })?;
    let mut max_record_bytes = 0;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_aggregate_value_records(left, right))
        {
            let record = readers[index].take_current()?;
            let record_bytes =
                write_json_spill_record(&mut writer, &record, "aggregate merge row")?;
            max_record_bytes = max_record_bytes.max(record_bytes);
        }
        writer.flush().map_err(|error| {
            SQLError::Internal(format!("failed to flush aggregate merge run: {error}"))
        })?;
    }
    output
        .as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| {
            SQLError::Internal(format!("failed to rewind aggregate merge run: {error}"))
        })?;
    Ok(JsonSpillRun {
        file: output,
        max_record_bytes,
    })
}

pub(in crate::sql) fn compare_aggregate_value_records(
    a: &AggregateValueRecord,
    b: &AggregateValueRecord,
) -> Ordering {
    for ((av, ad), (bv, _bd)) in a.sort_keys.iter().zip(b.sort_keys.iter()) {
        let cmp = av.cmp(bv);
        let cmp = if *ad { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.sequence.cmp(&b.sequence)
}
