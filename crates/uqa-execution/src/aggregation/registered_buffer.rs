//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spillable ordered input buffering for registered aggregates.

use super::ordering::{
    compare_sort_keys, compare_sort_keys_with_catalog, minimum_by, sort_records,
};
use uqa_sql::expr::enums::{EnumComparisonState, EnumLabelCatalog};

use super::{
    read_bounded_json_spill_record, write_json_spill_record, BufReader, BufWriter, File,
    JsonSpillRun, Ordering, SQLAggregateState, SQLError, Seek, SeekFrom, Value, Write,
    AGGREGATE_MERGE_FAN_IN,
};

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct RegisteredAggregateRecord {
    pub(super) values: Vec<Value>,
    pub(super) sort_keys: Vec<super::ordering::AggregateSortKey>,
    pub(super) sequence: u64,
}

pub struct RegisteredAggregateBuffer {
    pub(super) rows: Vec<RegisteredAggregateRecord>,
    pub(super) runs: Vec<JsonSpillRun>,
    pub(super) next_sequence: u64,
    pub(super) budget_bytes: usize,
    pub(super) memory_bytes: usize,
    pub(super) comparison_states: Option<std::sync::Arc<[EnumComparisonState]>>,
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
            comparison_states: None,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.runs.is_empty()
    }

    pub(super) fn push(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<super::ordering::AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        if !sort_keys.is_empty()
            && self.comparison_states.is_none()
            && enums.is_some_and(EnumLabelCatalog::has_enum_types)
        {
            self.comparison_states = Some(
                (0..sort_keys.len())
                    .map(|_| EnumComparisonState::default())
                    .collect(),
            );
        }
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
            self.flush_run(enums)?;
        }
        let next_memory_bytes = self
            .memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| SQLError::Internal("registered aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.memory_bytes = next_memory_bytes;
        self.next_sequence = next_sequence;
        if self.memory_bytes > self.budget_bytes {
            self.flush_run(enums)?;
        }
        Ok(())
    }

    pub(super) fn observe_ordered_into(
        &self,
        state: &mut dyn SQLAggregateState,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        if self.runs.is_empty() {
            let mut rows = self.rows.clone();
            sort_records(&mut rows, |left, right| {
                compare_registered_aggregate_records_with_catalog(
                    left,
                    right,
                    enums,
                    self.comparison_states.as_deref().unwrap_or_default(),
                )
            })?;
            for row in rows {
                state.observe(&row.values)?;
            }
            return Ok(());
        }

        let mut rows = self.rows.clone();
        sort_records(&mut rows, |left, right| {
            compare_registered_aggregate_records_with_catalog(
                left,
                right,
                enums,
                self.comparison_states.as_deref().unwrap_or_default(),
            )
        })?;
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!rows.is_empty()));
        if !rows.is_empty() {
            readers.push(RegisteredAggregateRunReader::memory(rows));
        }
        for run in &self.runs {
            readers.push(RegisteredAggregateRunReader::file(run)?);
        }

        while let Some((idx, _)) = minimum_by(
            readers
                .iter()
                .enumerate()
                .filter_map(|(idx, reader)| reader.current().map(|record| (idx, record))),
            |(_, a), (_, b)| {
                compare_registered_aggregate_records_with_catalog(
                    a,
                    b,
                    enums,
                    self.comparison_states.as_deref().unwrap_or_default(),
                )
            },
        )? {
            let record = readers[idx].take_current()?;
            state.observe(&record.values)?;
        }
        Ok(())
    }

    pub(super) fn flush_run(
        &mut self,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        sort_records(&mut self.rows, |left, right| {
            compare_registered_aggregate_records_with_catalog(
                left,
                right,
                enums,
                self.comparison_states.as_deref().unwrap_or_default(),
            )
        })?;
        let mut run = uqa_storage::temporary_file::TemporaryFile::new().map_err(|err| {
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
            self.runs.push(merge_registered_aggregate_runs_with_catalog(
                inputs,
                enums,
                self.comparison_states.as_deref().unwrap_or_default(),
            )?);
        }
        Ok(())
    }
}

pub enum RegisteredAggregateRunReader {
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

pub fn read_registered_aggregate_record(
    reader: &mut impl std::io::BufRead,
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

pub fn merge_registered_aggregate_runs(runs: Vec<JsonSpillRun>) -> Result<JsonSpillRun, SQLError> {
    merge_registered_aggregate_runs_with_catalog(runs, None, &[])
}

fn merge_registered_aggregate_runs_with_catalog(
    runs: Vec<JsonSpillRun>,
    enums: Option<&dyn EnumLabelCatalog>,
    states: &[EnumComparisonState],
) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(RegisteredAggregateRunReader::file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = uqa_storage::temporary_file::TemporaryFile::new().map_err(|error| {
        SQLError::Internal(format!(
            "failed to create registered aggregate merge run: {error}"
        ))
    })?;
    let mut max_record_bytes = 0;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        while let Some((index, _)) = minimum_by(
            readers
                .iter()
                .enumerate()
                .filter_map(|(index, reader)| reader.current().map(|record| (index, record))),
            |(_, left), (_, right)| {
                compare_registered_aggregate_records_with_catalog(left, right, enums, states)
            },
        )? {
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

pub fn compare_registered_aggregate_records(
    a: &RegisteredAggregateRecord,
    b: &RegisteredAggregateRecord,
) -> Result<Ordering, SQLError> {
    let ordering = compare_sort_keys(&a.sort_keys, &b.sort_keys)?;
    Ok(ordering.then_with(|| a.sequence.cmp(&b.sequence)))
}

fn compare_registered_aggregate_records_with_catalog(
    a: &RegisteredAggregateRecord,
    b: &RegisteredAggregateRecord,
    enums: Option<&dyn EnumLabelCatalog>,
    states: &[EnumComparisonState],
) -> Result<Ordering, SQLError> {
    let ordering = compare_sort_keys_with_catalog(&a.sort_keys, &b.sort_keys, enums, states)?;
    Ok(ordering.then_with(|| a.sequence.cmp(&b.sequence)))
}
