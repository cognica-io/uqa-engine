//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spillable ordered input buffering for registered aggregates.

use super::{
    read_bounded_json_spill_record, write_json_spill_record, BufReader, BufWriter, File,
    JsonSpillRun, Ordering, SQLAggregateState, SQLError, Seek, SeekFrom, Value, Write,
    AGGREGATE_MERGE_FAN_IN,
};

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(in crate::sql) struct RegisteredAggregateRecord {
    pub(super) values: Vec<Value>,
    pub(super) sort_keys: Vec<(Value, bool)>,
    pub(super) sequence: u64,
}

pub(in crate::sql) struct RegisteredAggregateBuffer {
    pub(super) rows: Vec<RegisteredAggregateRecord>,
    pub(super) runs: Vec<JsonSpillRun>,
    pub(super) next_sequence: u64,
    pub(super) budget_bytes: usize,
    pub(super) memory_bytes: usize,
}

impl Default for RegisteredAggregateBuffer {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl RegisteredAggregateBuffer {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            budget_bytes: budget_bytes.max(1),
            memory_bytes: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.runs.is_empty()
    }

    pub(super) fn push(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        let next_sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            SQLError::Internal("registered aggregate value sequence overflow".into())
        })?;
        let record = RegisteredAggregateRecord {
            values,
            sort_keys,
            sequence: self.next_sequence,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "failed to size registered aggregate value: {error}"
                ))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("registered aggregate value size overflow".into()))?;
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
            .ok_or_else(|| SQLError::Internal("registered aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.memory_bytes = next_memory_bytes;
        self.next_sequence = next_sequence;
        if self.memory_bytes > self.budget_bytes {
            self.flush_run()?;
        }
        Ok(())
    }

    pub(super) fn observe_ordered_into(
        &self,
        state: &mut dyn SQLAggregateState,
    ) -> Result<(), SQLError> {
        if self.runs.is_empty() {
            let mut rows = self.rows.clone();
            rows.sort_by(compare_registered_aggregate_records);
            for row in rows {
                state.observe(&row.values)?;
            }
            return Ok(());
        }

        let mut rows = self.rows.clone();
        rows.sort_by(compare_registered_aggregate_records);
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!rows.is_empty()));
        if !rows.is_empty() {
            readers.push(RegisteredAggregateRunReader::memory(rows));
        }
        for run in &self.runs {
            readers.push(RegisteredAggregateRunReader::file(run)?);
        }

        while let Some((idx, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(idx, reader)| reader.current().map(|record| (idx, record)))
            .min_by(|(_, a), (_, b)| compare_registered_aggregate_records(a, b))
        {
            let record = readers[idx].take_current()?;
            state.observe(&record.values)?;
        }
        Ok(())
    }

    pub(super) fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(compare_registered_aggregate_records);
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!(
                "failed to create registered aggregate spill file: {err}"
            ))
        })?;
        let mut max_record_bytes = 0;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                let record_bytes =
                    write_json_spill_record(&mut writer, &row, "registered aggregate spill row")?;
                max_record_bytes = max_record_bytes.max(record_bytes);
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!(
                    "failed to flush registered aggregate spill file: {err}"
                ))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!(
                "failed to rewind registered aggregate spill file: {err}"
            ))
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
            self.runs.push(merge_registered_aggregate_runs(inputs)?);
        }
        Ok(())
    }
}

pub(in crate::sql) enum RegisteredAggregateRunReader {
    Memory {
        rows: std::vec::IntoIter<RegisteredAggregateRecord>,
        current: Option<RegisteredAggregateRecord>,
    },
    File {
        reader: BufReader<File>,
        current: Option<RegisteredAggregateRecord>,
        max_record_bytes: usize,
    },
}

impl RegisteredAggregateRunReader {
    pub(super) fn memory(rows: Vec<RegisteredAggregateRecord>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    pub(super) fn file(run: &JsonSpillRun) -> Result<Self, SQLError> {
        let file = run.file.reopen().map_err(|err| {
            SQLError::Internal(format!(
                "failed to reopen registered aggregate spill file: {err}"
            ))
        })?;
        let mut reader = BufReader::new(file);
        let current = read_registered_aggregate_record(&mut reader, run.max_record_bytes)?;
        Ok(Self::File {
            reader,
            current,
            max_record_bytes: run.max_record_bytes,
        })
    }

    pub(super) fn current(&self) -> Option<&RegisteredAggregateRecord> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    pub(super) fn take_current(&mut self) -> Result<RegisteredAggregateRecord, SQLError> {
        match self {
            Self::Memory { rows, current } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate memory run exhausted".into())
                })?;
                *current = rows.next();
                Ok(record)
            }
            Self::File {
                reader,
                current,
                max_record_bytes,
            } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate spill run exhausted".into())
                })?;
                *current = read_registered_aggregate_record(reader, *max_record_bytes)?;
                Ok(record)
            }
        }
    }
}

pub(in crate::sql) fn read_registered_aggregate_record(
    reader: &mut BufReader<File>,
    max_record_bytes: usize,
) -> Result<Option<RegisteredAggregateRecord>, SQLError> {
    let Some(record) =
        read_bounded_json_spill_record(reader, max_record_bytes, "registered aggregate spill row")?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&record).map(Some).map_err(|err| {
        SQLError::Internal(format!(
            "failed to deserialize registered aggregate spill row: {err}"
        ))
    })
}

pub(in crate::sql) fn merge_registered_aggregate_runs(
    runs: Vec<JsonSpillRun>,
) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(RegisteredAggregateRunReader::file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = tempfile::NamedTempFile::new().map_err(|error| {
        SQLError::Internal(format!(
            "failed to create registered aggregate merge run: {error}"
        ))
    })?;
    let mut max_record_bytes = 0;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_registered_aggregate_records(left, right))
        {
            let record = readers[index].take_current()?;
            let record_bytes =
                write_json_spill_record(&mut writer, &record, "registered aggregate merge row")?;
            max_record_bytes = max_record_bytes.max(record_bytes);
        }
        writer.flush().map_err(|error| {
            SQLError::Internal(format!(
                "failed to flush registered aggregate merge run: {error}"
            ))
        })?;
    }
    output
        .as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| {
            SQLError::Internal(format!(
                "failed to rewind registered aggregate merge run: {error}"
            ))
        })?;
    Ok(JsonSpillRun {
        file: output,
        max_record_bytes,
    })
}

pub(in crate::sql) fn compare_registered_aggregate_records(
    a: &RegisteredAggregateRecord,
    b: &RegisteredAggregateRecord,
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
