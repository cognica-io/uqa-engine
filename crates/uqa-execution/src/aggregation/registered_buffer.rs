//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spillable ordered input buffering for registered aggregates.

use super::ordered_buffer::{self, OrderedRecord, OrderedRows, OrderedSpillBuffer, RunReader};
use super::ordering::{compare_sort_keys, AggregateSortKey};
use super::{read_bounded_json_spill_record, JsonSpillRun, SQLAggregateState};
use super::{Ordering, SQLError, Value};
use uqa_sql::expr::enums::EnumLabelCatalog;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct RegisteredAggregateRecord {
    pub(super) values: Vec<Value>,
    pub(super) sort_keys: Vec<super::ordering::AggregateSortKey>,
    pub(super) sequence: u64,
}

pub type RegisteredAggregateBuffer = OrderedSpillBuffer<RegisteredAggregateRecord>;
pub type RegisteredAggregateRunReader = RunReader<RegisteredAggregateRecord>;

impl OrderedRecord for RegisteredAggregateRecord {
    type Input = Vec<Value>;

    fn new(values: Self::Input, sort_keys: Vec<AggregateSortKey>, sequence: u64) -> Self {
        Self {
            values,
            sort_keys,
            sequence,
        }
    }

    fn inputs(&self) -> &[Value] {
        &self.values
    }

    fn input_payload_bytes(&self) -> usize {
        self.values
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(
                self.values
                    .iter()
                    .map(ordered_buffer::payload_bytes)
                    .fold(0usize, usize::saturating_add),
            )
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
        (0..count).map(|_| reader.read_value(0)).collect()
    }
}

impl OrderedRows<'_, RegisteredAggregateRecord> {
    pub(super) fn observe_ordered_into(
        self,
        state: &mut dyn SQLAggregateState,
        enums: Option<&dyn EnumLabelCatalog>,
    ) -> Result<(), SQLError> {
        self.for_each_ordered(enums, |record| state.observe(&record.values))
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
    ordered_buffer::merge_json::<RegisteredAggregateRecord>(&runs)
}

pub fn compare_registered_aggregate_records(
    left: &RegisteredAggregateRecord,
    right: &RegisteredAggregateRecord,
) -> Result<Ordering, SQLError> {
    Ok(compare_sort_keys(left.keys(), right.keys())?
        .then_with(|| left.sequence.cmp(&right.sequence)))
}
