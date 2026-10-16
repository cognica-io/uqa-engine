//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collision-safe in-memory and reusable exact row sets.

use crate::RowSchemaExecution;
use std::collections::HashMap;
use std::path::PathBuf;

use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::{ColumnType, ResultRow};

use crate::{ExecResult, PhysicalRow, RowSchema};

use super::encoding::{
    encode_key, encode_key_borrowed, encode_sql_key_with_types_and_catalog,
    hash_typed_row_with_catalog,
};
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
    types: Vec<Option<ColumnType>>,
}

impl CanonicalRowHashSet {
    #[must_use]
    pub fn new() -> Self {
        Self::with_sql_types(Vec::new())
    }

    /// Retain the selected key types for every insertion, hash probe and collision comparison.
    #[must_use]
    pub fn with_sql_types(types: Vec<Option<ColumnType>>) -> Self {
        Self {
            rows: Vec::new(),
            index: HashMap::with_hasher(ahash::RandomState::new()),
            types,
        }
    }

    /// Insert a positional key assembled from borrowed values.
    /// Returns `true` only when this is the first SQL-equal key.
    pub fn insert_borrowed(&mut self, values: &[&Value]) -> ExecResult<bool> {
        self.insert_borrowed_with_catalog(values, None)
    }

    pub fn insert_borrowed_with_catalog(
        &mut self,
        values: &[&Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let hash = self.hash_values(values.iter().copied(), catalog)?;
        if self.matching(hash, values.iter().copied(), catalog)? {
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
        self.insert_values_with_catalog(values, None)
    }

    pub fn insert_values_with_catalog(
        &mut self,
        values: &[Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let hash = self.hash_values(values.iter(), catalog)?;
        if self.matching(hash, values.iter(), catalog)? {
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
        self.contains_borrowed_with_catalog(values, None)
    }

    pub fn contains_borrowed_with_catalog(
        &self,
        values: &[&Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let hash = self.hash_values(values.iter().copied(), catalog)?;
        self.matching(hash, values.iter().copied(), catalog)
    }

    /// Probe with an already positional value slice.
    pub fn contains_values(&self, values: &[Value]) -> ExecResult<bool> {
        self.contains_values_with_catalog(values, None)
    }

    pub fn contains_values_with_catalog(
        &self,
        values: &[Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let hash = self.hash_values(values.iter(), catalog)?;
        self.matching(hash, values.iter(), catalog)
    }

    fn hash_values<'a>(
        &'a self,
        values: impl ExactSizeIterator<Item = &'a Value>,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<u64> {
        hash_typed_row_with_catalog(
            self.index.hasher(),
            values.enumerate().map(|(index, value)| {
                (Some(value), self.types.get(index).and_then(Option::as_ref))
            }),
            catalog,
        )
    }

    fn matching<'a>(
        &self,
        hash: u64,
        values: impl ExactSizeIterator<Item = &'a Value> + Clone,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let Some(bucket) = self.index.get(&hash) else {
            return Ok(false);
        };
        for index in bucket {
            let stored = &self.rows[*index];
            if stored.len() != values.len() {
                continue;
            }
            let mut equal = true;
            for (position, (stored, value)) in stored.iter().zip(values.clone()).enumerate() {
                if !uqa_sql::expr::equal_selected_values_with_catalog(
                    stored,
                    value,
                    self.types.get(position).and_then(Option::as_ref),
                    &uqa_core::memory::ProductionControl::uncontrolled(),
                    catalog,
                )? {
                    equal = false;
                    break;
                }
            }
            if equal {
                return Ok(true);
            }
        }
        Ok(false)
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
    types: Option<Vec<Option<ColumnType>>>,
}

impl ExactRowSet {
    /// The first SQL insertion selects the key types; probes do not bind an empty set. Explicit selected types can instead be supplied by `with_sql_types`.
    pub fn new(work_mem_bytes: usize) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, None),
            types: None,
        }
    }

    /// Keep the selected SQL value-key types across memory-to-disk migration and later probes. Raw-key methods retain their existing representation domain.
    pub fn with_sql_types(work_mem_bytes: usize, types: Vec<Option<ColumnType>>) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, None),
            types: Some(types),
        }
    }

    pub fn with_spill_directory(work_mem_bytes: usize, directory: impl Into<PathBuf>) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, Some(directory.into())),
            types: None,
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

    /// Insert selected SQL keys with the statement's retained type catalog.
    pub fn insert_sql_values(
        &mut self,
        values: &[Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        self.types.get_or_insert_with(Vec::new);
        self.seen.insert(self.sql_key(values, catalog)?)
    }

    /// Probe the same SQL equality domain used during insertion, including after spill.
    pub fn contains_sql_values(
        &mut self,
        values: &[Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        self.seen.contains(&self.sql_key(values, catalog)?)
    }

    fn sql_key(
        &self,
        values: &[Value],
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<Vec<u8>> {
        let types = self.types.as_deref().unwrap_or_default();
        encode_sql_key_with_types_and_catalog(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (Some(value), types.get(index).and_then(Option::as_ref))),
            catalog,
        )
    }

    /// Insert SQL fields in logical schema order without cloning their payloads. An unbound set captures this schema's selected types once; every later insertion and probe uses the captured or explicitly selected types.
    pub fn insert_sql_physical(
        &mut self,
        row: &PhysicalRow,
        schema: &RowSchema,
        catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
    ) -> ExecResult<bool> {
        let types = self.types.get_or_insert_with(|| {
            (0..schema.len())
                .map(|position| schema.column_type(position).cloned())
                .collect()
        });
        let view = schema.view(row);
        self.seen.insert(encode_sql_key_with_types_and_catalog(
            (0..schema.len()).map(|position| {
                (
                    view.value_at(position),
                    types.get(position).and_then(Option::as_ref),
                )
            }),
            catalog,
        )?)
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
