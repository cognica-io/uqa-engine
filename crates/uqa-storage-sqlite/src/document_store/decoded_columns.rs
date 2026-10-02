//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded scalar columns of latest committed native documents, shared by the readers of one database.
//!
//! An entry belongs to one table owner at one data generation. The `_documents` triggers advance a table's data generation in the transaction of every row change, and owner identities are never reused, so a latest-commit read that observes an entry's owner and generation observes exactly the rows the entry was decoded from. Only null, boolean, integer, float and string values are cached: their retained sizes are exact, and a value stored outside the document body is never one of them.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError, MemoryReservation};
use uqa_core::Value;

use crate::mvcc::native::NativeRecordOwner;

mod builder;
pub(crate) use builder::ColumnBuilder;

/// Allowance for the decoded columns of one database.
pub(crate) const DEFAULT_LIMIT: usize = 64 * 1024 * 1024;

/// Scans paused between pages whose builds may still continue.
const PARKED_BUILDS: usize = 4;

/// Document identities and, for each requested field, its values aligned with them.
pub(crate) type CachedColumns = (Arc<Vec<i64>>, Vec<Arc<Vec<Value>>>);

pub(crate) struct DecodedColumns {
    budget: MemoryBudget,
    /// Least recently used first.
    entries: Mutex<Vec<Entry>>,
    /// Builds of paged scans, oldest first, waiting for the page that continues them.
    parked: Mutex<Vec<ColumnBuilder>>,
}

struct Entry {
    table: String,
    owner: NativeRecordOwner,
    generation: i64,
    ids: Arc<Vec<i64>>,
    columns: Vec<(String, Arc<Vec<Value>>)>,
    memory: Vec<MemoryReservation>,
}

impl Entry {
    fn matches(&self, table: &str, owner: NativeRecordOwner, generation: i64) -> bool {
        self.table == table && self.owner == owner && self.generation == generation
    }
}

impl DecodedColumns {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            budget: MemoryBudget::new(limit),
            entries: Mutex::new(Vec::new()),
            parked: Mutex::new(Vec::new()),
        }
    }

    /// Builders charge this allowance from their first row.
    pub(crate) fn budget(&self) -> &MemoryBudget {
        &self.budget
    }

    /// The columns of every requested field of `table` at `generation`, when all of them are cached.
    pub(crate) fn get(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        generation: i64,
        fields: &[&str],
    ) -> Option<CachedColumns> {
        let mut entries = self.entries.lock();
        let position = entries
            .iter()
            .position(|entry| entry.matches(table, owner, generation))?;
        let entry = entries.remove(position);
        let columns = fields
            .iter()
            .map(|field| {
                entry
                    .columns
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, values)| Arc::clone(values))
            })
            .collect::<Option<Vec<_>>>();
        let found = columns.map(|columns| (Arc::clone(&entry.ids), columns));
        entries.push(entry);
        found
    }

    /// Drop the entries of `table` other than `owner` at `generation`, which no later read can observe.
    pub(crate) fn retire_stale(&self, table: &str, owner: NativeRecordOwner, generation: i64) {
        self.entries
            .lock()
            .retain(|entry| entry.table != table || entry.matches(table, owner, generation));
    }

    /// Evict the oldest parked build or least recently used entry other than `table`'s, returning whether one was evicted.
    pub(crate) fn evict_other(&self, table: &str) -> bool {
        {
            let mut parked = self.parked.lock();
            if let Some(position) = parked.iter().position(|build| build.table != table) {
                parked.remove(position);
                return true;
            }
        }
        let mut entries = self.entries.lock();
        let Some(position) = entries.iter().position(|entry| entry.table != table) else {
            return false;
        };
        entries.remove(position);
        true
    }

    /// Take the parked build that a page of `table` selecting `fields` after `after` continues.
    pub(crate) fn resume(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        generation: i64,
        fields: &[&str],
        after: Option<i64>,
    ) -> Option<ColumnBuilder> {
        let mut parked = self.parked.lock();
        let position = parked
            .iter()
            .position(|build| build.continues(table, owner, generation, fields, after))?;
        Some(parked.remove(position))
    }

    /// Keep a build whose scan stopped at a page boundary for the page that continues it.
    pub(crate) fn park(&self, build: ColumnBuilder) {
        let mut parked = self.parked.lock();
        if parked.len() == PARKED_BUILDS {
            parked.remove(0);
        }
        parked.push(build);
    }

    /// Record the columns of a scan that reached the table's last row. Fields already cached for the same rows keep their columns.
    pub(crate) fn finish(&self, build: ColumnBuilder) {
        let columns = build.fields.into_iter().zip(build.columns).collect();
        self.insert(
            &build.table,
            build.owner,
            build.generation,
            build.ids,
            columns,
            build.strings,
        );
    }

    /// Record complete columns of `table` decoded at `generation`. Their buffers and string payloads are already charged to [`Self::budget`].
    fn insert(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        generation: i64,
        ids: BudgetedVec<i64>,
        columns: Vec<(String, BudgetedVec<Value>)>,
        strings: MemoryReservation,
    ) {
        let mut entries = self.entries.lock();
        entries.retain(|entry| entry.table != table || entry.matches(table, owner, generation));
        let mut memory = vec![strings];
        let position = entries
            .iter()
            .position(|entry| entry.matches(table, owner, generation));
        let mut entry = if let Some(position) = position {
            entries.remove(position)
        } else {
            let (ids, reservation) = ids.into_parts();
            memory.push(reservation);
            Entry {
                table: table.to_owned(),
                owner,
                generation,
                ids: Arc::new(ids),
                columns: Vec::new(),
                memory: Vec::new(),
            }
        };
        for (field, values) in columns {
            if entry.columns.iter().any(|(name, _)| *name == field) {
                continue;
            }
            let (values, reservation) = values.into_parts();
            memory.push(reservation);
            entry.columns.push((field, Arc::new(values)));
        }
        entry.memory.extend(memory);
        entries.push(entry);
    }
}

/// Retained heap bytes of a cacheable value, or `None` for a value the cache does not hold.
pub(crate) fn scalar_bytes(value: &Value) -> Option<usize> {
    match value {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => Some(0),
        Value::Str(text) => Some(text.capacity()),
        _ => None,
    }
}

/// Charge `bytes` to a builder's allowance, evicting other tables' entries to make room.
pub(crate) fn grow(
    cache: &DecodedColumns,
    table: &str,
    memory: &mut MemoryReservation,
    bytes: usize,
) -> Result<(), MemoryError> {
    loop {
        match memory.grow(bytes) {
            Ok(()) => return Ok(()),
            Err(error) if !cache.evict_other(table) => return Err(error),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
