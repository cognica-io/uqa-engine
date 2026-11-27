//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered input buffering shared by builtin and registered aggregates.

use super::ordering::{compare_sort_keys_with_catalog, sort_records, AggregateSortKey};
pub(super) use super::retained_size::payload_bytes;
use super::{JsonSpillRun, AGGREGATE_MERGE_FAN_IN};
use crate::spill::merge_heap;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::io::{BufReader, BufWriter, Write};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::expr::enums::{EnumComparisonState, EnumLabelCatalog};
use uqa_sql::{ColumnType, SQLError};
use uqa_storage::temporary_file::TemporaryFile;

mod codec;

pub(super) trait OrderedRecord:
    serde::Serialize + serde::de::DeserializeOwned + Clone
{
    type Input;
    fn new(value: Self::Input, keys: Vec<AggregateSortKey>, sequence: u64) -> Self;
    fn inputs(&self) -> &[Value];
    fn input_payload_bytes(&self) -> usize;
    fn keys(&self) -> &Vec<AggregateSortKey>;
    fn sequence(&self) -> u64;
    fn read_input(
        reader: &mut crate::spill::BinaryReader<'_>,
        count: usize,
    ) -> crate::ExecResult<Self::Input>;
}

fn retained_bytes<R: OrderedRecord>(record: &R) -> usize {
    let keys = record.keys();
    let slots = keys
        .capacity()
        .saturating_mul(std::mem::size_of::<AggregateSortKey>());
    keys.iter().fold(
        std::mem::size_of::<R>()
            .saturating_add(record.input_payload_bytes())
            .saturating_add(slots),
        |total, key| total.saturating_add(payload_bytes(&key.value)),
    )
}

pub struct OrderedSpillBuffer<R> {
    pub(super) rows: Vec<R>,
    pub(super) runs: Vec<JsonSpillRun>,
    pub(super) next_sequence: u64,
    pub(super) budget_bytes: usize,
    pub(super) memory_bytes: usize,
    pub(super) comparison_states: Option<Arc<[EnumComparisonState]>>,
    pub(super) key_types: Option<Arc<[Option<ColumnType>]>>,
}

impl<R> Default for OrderedSpillBuffer<R> {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl<R> OrderedSpillBuffer<R> {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            budget_bytes: budget_bytes.max(1),
            memory_bytes: 0,
            comparison_states: None,
            key_types: None,
        }
    }

    pub(super) fn push(
        &mut self,
        value: R::Input,
        keys: Vec<AggregateSortKey>,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError>
    where
        R: OrderedRecord,
    {
        if !keys.is_empty()
            && self.comparison_states.is_none()
            && enums.is_some_and(EnumLabelCatalog::has_enum_types)
        {
            self.comparison_states = Some(
                (0..keys.len())
                    .map(|_| EnumComparisonState::default())
                    .collect(),
            );
        }
        let next = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate value sequence overflow".into()))?;
        let record = R::new(value, keys, self.next_sequence);
        let bytes = retained_bytes(&record);
        if !self.rows.is_empty()
            && self
                .memory_bytes
                .checked_add(bytes)
                .is_none_or(|v| v > self.budget_bytes)
        {
            self.flush_run(enums)?;
        }
        self.memory_bytes = self
            .memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| SQLError::Internal("aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.next_sequence = next;
        if self.memory_bytes > self.budget_bytes {
            self.flush_run(enums)?;
        }
        Ok(())
    }

    pub(super) fn flush_run(&mut self, enums: Option<&dyn EnumLabelCatalog>) -> Result<(), SQLError>
    where
        R: OrderedRecord,
    {
        if self.rows.is_empty() {
            return Ok(());
        }
        let order = RecordOrder::new(
            enums,
            self.comparison_states.as_deref(),
            self.key_types.as_deref(),
        );
        sort_records(&mut self.rows, |a, b| order.compare(a, b))?;
        let run = write_run(|writer, max| {
            for record in &self.rows {
                *max = (*max).max(codec::write(writer, record)?);
            }
            Ok(())
        })?;
        self.rows.clear();
        self.memory_bytes = 0;
        self.runs.push(run);
        if self.runs.len() >= AGGREGATE_MERGE_FAN_IN {
            let merged = merge_runs::<R>(&self.runs, &order, Encoding::Binary)?;
            self.runs.clear();
            self.runs.push(merged);
        }
        Ok(())
    }

    pub(super) fn input(&self) -> OrderedRows<'_, R>
    where
        R: Clone,
    {
        self.with_rows(self.rows.as_slice())
    }

    pub(super) fn with_rows<'a>(&'a self, rows: impl Into<Cow<'a, [R]>>) -> OrderedRows<'a, R>
    where
        R: Clone,
    {
        OrderedRows {
            buffer: self,
            rows: rows.into(),
        }
    }

    fn visit_rows(
        &self,
        mut rows: Vec<R>,
        enums: Option<&dyn EnumLabelCatalog>,
        mut visit: impl FnMut(R) -> Result<(), SQLError>,
    ) -> Result<(), SQLError>
    where
        R: OrderedRecord,
    {
        let order = RecordOrder::new(
            enums,
            self.comparison_states.as_deref(),
            self.key_types.as_deref(),
        );
        sort_records(&mut rows, |a, b| order.compare(a, b))?;
        if self.runs.is_empty() {
            return rows.into_iter().try_for_each(visit);
        }
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!rows.is_empty()));
        if !rows.is_empty() {
            readers.push(RunReader::memory(rows));
        }
        for run in &self.runs {
            readers.push(RunReader::file(run, Encoding::Binary)?);
        }
        merge_readers(
            &mut readers,
            |a, b| order.compare(a, b),
            Encoding::Binary,
            &mut visit,
        )
    }
}

/// A completed group transfers its row allocations. A window frame keeps its rows for later reads.
pub(super) struct OrderedRows<'a, R: Clone> {
    pub(super) buffer: &'a OrderedSpillBuffer<R>,
    rows: Cow<'a, [R]>,
}

impl<R: OrderedRecord> OrderedRows<'_, R> {
    pub(super) fn for_each_ordered(
        self,
        enums: Option<&dyn EnumLabelCatalog>,
        visit: impl FnMut(R) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.buffer.visit_rows(self.rows.into_owned(), enums, visit)
    }
}

struct RecordOrder<'a> {
    enums: Option<&'a dyn EnumLabelCatalog>,
    states: &'a [EnumComparisonState],
    types: &'a [Option<ColumnType>],
}

impl<'a> RecordOrder<'a> {
    fn new(
        enums: Option<&'a dyn EnumLabelCatalog>,
        states: Option<&'a [EnumComparisonState]>,
        types: Option<&'a [Option<ColumnType>]>,
    ) -> Self {
        Self {
            enums,
            states: states.unwrap_or_default(),
            types: types.unwrap_or_default(),
        }
    }
    fn compare<R: OrderedRecord>(&self, a: &R, b: &R) -> Result<Ordering, SQLError> {
        Ok(
            compare_sort_keys_with_catalog(
                a.keys(),
                b.keys(),
                self.enums,
                self.states,
                self.types,
            )?
            .then_with(|| a.sequence().cmp(&b.sequence())),
        )
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Json,
    Binary,
}

pub enum RunReader<R> {
    Memory {
        rows: std::vec::IntoIter<R>,
        current: Option<R>,
    },
    File {
        reader: BufReader<TemporaryFile>,
        current: Option<R>,
        max_record_bytes: usize,
    },
}

impl<R> RunReader<R> {
    fn memory(rows: Vec<R>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    fn file(run: &JsonSpillRun, encoding: Encoding) -> Result<Self, SQLError>
    where
        R: OrderedRecord,
    {
        let file = run.file.reopen().map_err(|err| {
            SQLError::Internal(format!("failed to reopen aggregate spill file: {err}"))
        })?;
        let mut reader = BufReader::new(file);
        let current = encoding.read(&mut reader, run.max_record_bytes)?;
        Ok(Self::File {
            reader,
            current,
            max_record_bytes: run.max_record_bytes,
        })
    }

    fn current(&self) -> Option<&R> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    fn take(&mut self, encoding: Encoding) -> Result<R, SQLError>
    where
        R: OrderedRecord,
    {
        let (current, next) = match self {
            Self::Memory { rows, current } => (current, rows.next()),
            Self::File {
                reader,
                current,
                max_record_bytes,
            } => (current, encoding.read(reader, *max_record_bytes)?),
        };
        Ok(std::mem::replace(current, next).expect("only active run heads enter the heap"))
    }
}

impl Encoding {
    fn read<R: OrderedRecord>(
        self,
        reader: &mut impl std::io::BufRead,
        max: usize,
    ) -> Result<Option<R>, SQLError> {
        match self {
            Self::Json => {
                super::read_bounded_json_spill_record(reader, max, "aggregate spill row")?
                    .map(|bytes| {
                        serde_json::from_slice(&bytes).map_err(|error| {
                            SQLError::Internal(format!(
                                "failed to deserialize aggregate spill row: {error}"
                            ))
                        })
                    })
                    .transpose()
            }
            Self::Binary => codec::read(reader, max),
        }
    }
}

fn merge_readers<R: OrderedRecord>(
    readers: &mut [RunReader<R>],
    compare_records: impl Fn(&R, &R) -> Result<Ordering, SQLError>,
    encoding: Encoding,
    mut visit: impl FnMut(R) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let compare = |readers: &[RunReader<R>], a: &usize, b: &usize| {
        compare_records(
            readers[*a].current().expect("active run"),
            readers[*b].current().expect("active run"),
        )
        .map(|ordering| ordering.then_with(|| a.cmp(b)))
    };
    let mut heap = Vec::with_capacity(readers.len());
    for (i, reader) in readers.iter().enumerate() {
        if reader.current().is_some() {
            merge_heap::push(&mut heap, i, |a, b| compare(readers, a, b))?;
        }
    }
    while let Some(&index) = heap.first() {
        visit(readers[index].take(encoding)?)?;
        if readers[index].current().is_none() {
            heap.swap_remove(0);
        }
        merge_heap::sift_down(&mut heap, 0, |a, b| compare(readers, a, b))?;
    }
    Ok(())
}

fn write_run(
    write: impl FnOnce(&mut BufWriter<&mut TemporaryFile>, &mut usize) -> Result<(), SQLError>,
) -> Result<JsonSpillRun, SQLError> {
    let mut file = TemporaryFile::new()
        .map_err(|e| SQLError::Internal(format!("failed to create aggregate spill file: {e}")))?;
    let mut max_record_bytes = 0;
    let mut writer = BufWriter::new(file.as_file_mut());
    write(&mut writer, &mut max_record_bytes)?;
    writer
        .flush()
        .map_err(|e| SQLError::Internal(format!("failed to flush aggregate spill file: {e}")))?;
    drop(writer);
    Ok(JsonSpillRun {
        file,
        max_record_bytes,
    })
}

fn merge_runs<R: OrderedRecord>(
    runs: &[JsonSpillRun],
    order: &RecordOrder<'_>,
    encoding: Encoding,
) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(|run| RunReader::<R>::file(run, encoding))
        .collect::<Result<Vec<_>, _>>()?;
    write_run(|writer, max| {
        merge_readers(
            &mut readers,
            |a, b| order.compare(a, b),
            encoding,
            |record| {
                let bytes = match encoding {
                    Encoding::Json => {
                        super::write_json_spill_record(writer, &record, "aggregate merge row")?
                    }
                    Encoding::Binary => codec::write(writer, &record)?,
                };
                *max = (*max).max(bytes);
                Ok(())
            },
        )
    })
}

pub(super) fn merge_json<R: OrderedRecord>(
    runs: &[JsonSpillRun],
) -> Result<JsonSpillRun, SQLError> {
    merge_runs::<R>(runs, &RecordOrder::new(None, None, None), Encoding::Json)
}

#[cfg(test)]
mod tests;
