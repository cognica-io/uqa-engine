//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collision-safe in-memory and reusable exact row sets.

use std::collections::HashMap;
use std::path::PathBuf;

use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::ResultRow;

use crate::{ExecResult, PhysicalRow, RowSchema};

use super::encoding::{encode_key, encode_key_borrowed, hash_canonical_row};
use super::spill::SeenKeySet;

/// Collision-safe in-memory set for positional SQL rows.
///
/// Probes consume borrowed values and stream their canonical representation
/// directly into the hash function. Only the first distinct row is copied
/// into the contiguous key arena; repeated build rows and every lookup avoid
/// both a positional `Vec<Value>` allocation and value cloning. Hash matches
/// always verify the complete SQL [`Value`] equality domain.
pub struct CanonicalRowHashSet {
    pub(super) rows: Vec<SmallVec<[Value; 2]>>,
    index: HashMap<u64, SmallVec<[usize; 1]>, ahash::RandomState>,
}

impl CanonicalRowHashSet {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            index: HashMap::with_hasher(ahash::RandomState::new()),
        }
    }

    /// Insert a positional key assembled from borrowed values.
    /// Returns `true` only when this is the first SQL-equal key.
    pub fn insert_borrowed(&mut self, values: &[&Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().copied().map(Some))?;
        if self.matching_borrowed(hash, values) {
            return Ok(false);
        }

        let row = values
            .iter()
            .map(|value| (*value).clone())
            .collect::<SmallVec<[Value; 2]>>();
        let row_index = self.rows.len();
        self.rows.push(row);
        self.index.entry(hash).or_default().push(row_index);
        Ok(true)
    }

    /// Insert an already positional key without an intermediate borrowed-row
    /// carrier. Values are copied only for a previously unseen key.
    pub fn insert_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().map(Some))?;
        if self.matching_values(hash, values) {
            return Ok(false);
        }

        let row_index = self.rows.len();
        self.rows.push(values.iter().cloned().collect());
        self.index.entry(hash).or_default().push(row_index);
        Ok(true)
    }

    /// Probe with a composite row of borrowed values without allocating or
    /// copying the key.
    pub fn contains_borrowed(&self, values: &[&Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().copied().map(Some))?;
        Ok(self.matching_borrowed(hash, values))
    }

    /// Probe with an already positional value slice.
    pub fn contains_values(&self, values: &[Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().map(Some))?;
        Ok(self.matching_values(hash, values))
    }

    fn matching_borrowed(&self, hash: u64, values: &[&Value]) -> bool {
        self.index.get(&hash).is_some_and(|bucket| {
            bucket.iter().copied().any(|index| {
                let stored = &self.rows[index];
                stored.len() == values.len()
                    && stored
                        .iter()
                        .zip(values)
                        .all(|(stored, value)| stored == *value)
            })
        })
    }

    fn matching_values(&self, hash: u64, values: &[Value]) -> bool {
        self.index.get(&hash).is_some_and(|bucket| {
            bucket
                .iter()
                .copied()
                .any(|index| self.rows[index].as_slice() == values)
        })
    }
}

impl Default for CanonicalRowHashSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact, byte-bounded row-key set that can outlive one physical operator.
///
/// Recursive fixpoint evaluation needs duplicate state to survive across
/// multiple executions of its recursive term. [`Distinct`](crate::distinct::Distinct) deliberately
/// resets its state on every `open`, so this small public carrier exposes the
/// same collision-safe memory-to-disk migration without coupling the engine to
/// the on-disk format.
pub struct ExactRowSet {
    seen: SeenKeySet,
}

impl ExactRowSet {
    pub fn new(work_mem_bytes: usize) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, None),
        }
    }

    pub fn with_spill_directory(work_mem_bytes: usize, directory: impl Into<PathBuf>) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, Some(directory.into())),
        }
    }

    /// Insert the positional values from `row` in `schema` order.
    /// Returns `true` only for the first exact occurrence.
    pub fn insert_row(&mut self, row: &ResultRow, schema: &[String]) -> ExecResult<bool> {
        self.seen.insert(row_key(row, schema)?)
    }

    pub fn contains_row(&mut self, row: &ResultRow, schema: &[String]) -> ExecResult<bool> {
        self.seen.contains(&row_key(row, schema)?)
    }

    /// Insert an already-positional SQL value key without constructing a
    /// named row. The binary encoding is the same collision-safe,
    /// cross-numeric representation used by physical DISTINCT.
    pub fn insert_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        self.seen.insert(encode_key(values)?)
    }

    /// Probe an already-positional SQL value key without constructing a named
    /// row. Disk-backed sets perform an exact full-key comparison.
    pub fn contains_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        self.seen.contains(&encode_key(values)?)
    }

    /// Insert a physical row directly in logical schema order without constructing a named row or cloning its values.
    pub fn insert_physical(&mut self, row: &PhysicalRow, schema: &RowSchema) -> ExecResult<bool> {
        let view = schema.view(row);
        self.seen.insert(encode_key_borrowed(
            (0..schema.len()).map(|position| view.value_at(position)),
        )?)
    }

    /// Probe a physical row directly in logical schema order without constructing a named row or cloning its values.
    pub fn contains_physical(&mut self, row: &PhysicalRow, schema: &RowSchema) -> ExecResult<bool> {
        let view = schema.view(row);
        self.seen.contains(&encode_key_borrowed(
            (0..schema.len()).map(|position| view.value_at(position)),
        )?)
    }

    pub fn has_spilled(&self) -> bool {
        self.seen.has_spilled()
    }

    pub fn in_memory_key_bytes(&self) -> usize {
        self.seen.in_memory_bytes()
    }
}

fn row_key(row: &ResultRow, schema: &[String]) -> ExecResult<Vec<u8>> {
    encode_key_borrowed(schema.iter().map(|column| row.get(column)))
}
