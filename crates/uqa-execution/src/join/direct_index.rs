//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free direct index for simple positional equality keys.

use crate::RowSchemaExecution;
use std::collections::{hash_map::Entry, HashMap};
use std::hash::BuildHasher;

use smallvec::SmallVec;
use uqa_core::Value;

use crate::distinct::hash_typed_row_with_collations;
use crate::{ExecResult, PhysicalRowView, RowSchema};
use uqa_sql::expr::enums::EnumLabelCatalog;

use super::row_store::HybridRowStore;

/// Allocation-free in-memory index for simple positional equality keys.
///
/// Only the canonical hash and build-row position are retained. The key itself stays in the original [`crate::PhysicalRow`]; hash collisions are resolved by comparing the mapped source slots. If the index exceeds its budget, its state is discarded and the caller rebuilds the spill-capable encoded index from the row store.
pub(super) struct DirectHashIndex {
    buckets: HashMap<u64, SmallVec<[u64; 1]>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    overflowed: bool,
}

impl DirectHashIndex {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            buckets: HashMap::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            overflowed: false,
        }
    }

    pub(super) fn hasher(&self) -> &ahash::RandomState {
        self.buckets.hasher()
    }

    pub(super) fn insert(&mut self, hash: u64, row_index: u64) {
        if self.overflowed {
            return;
        }
        let entry = self.buckets.entry(hash);
        // Include the hash, inline index, control bytes and table slack once per bucket.
        let record_bytes = if matches!(entry, Entry::Occupied(_)) {
            8
        } else {
            64
        };
        if let Some(bytes) = self
            .memory_bytes
            .checked_add(record_bytes)
            .filter(|bytes| *bytes <= self.budget_bytes)
        {
            entry.or_default().push(row_index);
            self.memory_bytes = bytes;
        } else {
            self.buckets = HashMap::with_hasher(self.buckets.hasher().clone());
            self.memory_bytes = 0;
            self.overflowed = true;
        }
    }

    pub(super) fn is_available(&self) -> bool {
        !self.overflowed
    }

    pub(super) fn candidates(&self, hash: u64) -> &[u64] {
        self.buckets.get(&hash).map_or(&[], SmallVec::as_slice)
    }

    pub(super) fn keys_are_unique(
        &self,
        rows: &HybridRowStore,
        schema: &RowSchema,
        positions: &[usize],
        catalog: Option<&dyn EnumLabelCatalog>,
    ) -> ExecResult<bool> {
        if !self.is_available() {
            return Ok(false);
        }
        for bucket in self.buckets.values() {
            for (offset, left_index) in bucket.iter().enumerate() {
                for right_index in &bucket[offset + 1..] {
                    let (Some(left), Some(right)) =
                        (rows.memory_row(*left_index), rows.memory_row(*right_index))
                    else {
                        return Ok(false);
                    };
                    if positional_keys_equal(
                        schema.view(left),
                        positions,
                        schema.view(right),
                        positions,
                        catalog,
                    )? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }
}

pub(super) fn positional_key_hash<S: BuildHasher>(
    build_hasher: &S,
    view: PhysicalRowView<'_>,
    positions: &[usize],
    indeterminate: &[usize],
    catalog: Option<&dyn EnumLabelCatalog>,
) -> ExecResult<Option<u64>> {
    if positions.iter().any(|position| {
        view.value_at(*position)
            .is_none_or(|value| matches!(value, Value::Null))
    }) {
        return Ok(None);
    }
    hash_typed_row_with_collations(
        build_hasher,
        positions
            .iter()
            .map(|position| (view.value_at(*position), view.column_type(*position))),
        indeterminate,
        catalog,
    )
    .map(Some)
}

fn positional_keys_equal(
    left: PhysicalRowView<'_>,
    left_positions: &[usize],
    right: PhysicalRowView<'_>,
    right_positions: &[usize],
    catalog: Option<&dyn EnumLabelCatalog>,
) -> ExecResult<bool> {
    if left_positions.len() != right_positions.len() {
        return Ok(false);
    }
    for (left_position, right_position) in left_positions.iter().zip(right_positions) {
        let (Some(left_value), Some(right_value)) = (
            left.value_at(*left_position),
            right.value_at(*right_position),
        ) else {
            return Ok(false);
        };
        if !uqa_sql::expr::equal_selected_values_with_catalog(
            left_value,
            right_value,
            left.column_type(*left_position)
                .or_else(|| right.column_type(*right_position)),
            &uqa_core::memory::ProductionControl::uncontrolled(),
            catalog,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Resumable position in the original build bucket. The probe keeps its SQL row for exact collision checks.
#[derive(Default)]
pub(super) struct DirectMatches {
    hash: Option<u64>,
    offset: usize,
}

impl DirectMatches {
    pub(super) fn for_probe(
        index: &DirectHashIndex,
        probe: PhysicalRowView<'_>,
        positions: &[usize],
        indeterminate: &[usize],
        catalog: Option<&dyn EnumLabelCatalog>,
    ) -> ExecResult<Self> {
        let hash = positional_key_hash(index.hasher(), probe, positions, indeterminate, catalog)?;
        Ok(Self { hash, offset: 0 })
    }

    pub(super) fn next(
        &mut self,
        index: &DirectHashIndex,
        build_rows: &HybridRowStore,
        build_positions: &[usize],
        probe: PhysicalRowView<'_>,
        probe_positions: &[usize],
        catalog: Option<&dyn EnumLabelCatalog>,
    ) -> ExecResult<Option<u64>> {
        let Some(hash) = self.hash else {
            return Ok(None);
        };
        let candidates = index.candidates(hash);
        while let Some(&position) = candidates.get(self.offset) {
            self.offset += 1;
            if let Some(row) = build_rows.memory_row(position) {
                if positional_keys_equal(
                    build_rows.schema.view(row),
                    build_positions,
                    probe,
                    probe_positions,
                    catalog,
                )? {
                    return Ok(Some(position));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PhysicalRow;

    #[test]
    fn direct_probe_resumes_duplicate_positions_and_rejects_hash_collisions() {
        let schema = RowSchema::new(vec!["key".into()]);
        let mut rows = HybridRowStore::new(schema.clone(), usize::MAX);
        let mut index = DirectHashIndex::new(usize::MAX);
        let probe = PhysicalRow::from_values(vec![Value::Int(7)]);
        let probe = schema.view(&probe);
        let mut matches = DirectMatches::for_probe(&index, probe, &[0], &[], None).unwrap();
        let hash = matches.hash.unwrap();
        for value in [7, 8, 7, 9, 7] {
            index.insert(hash, rows.len());
            rows.push(PhysicalRow::from_values(vec![Value::Int(value)]))
                .unwrap();
        }
        for expected in [Some(0), Some(2), Some(4), None, None] {
            assert_eq!(
                matches
                    .next(&index, &rows, &[0], probe, &[0], None)
                    .unwrap(),
                expected
            );
        }
        let null = PhysicalRow::from_values(vec![Value::Null]);
        let null = schema.view(&null);
        let mut absent = DirectMatches::for_probe(&index, null, &[0], &[], None).unwrap();
        assert_eq!(
            absent.next(&index, &rows, &[0], null, &[0], None).unwrap(),
            None
        );
    }

    #[test]
    fn positional_join_datum_keys_use_sql_equality_and_preserve_errors() {
        let schema = RowSchema::new(vec!["key".into()]);
        let old = PhysicalRow::from_values(vec![Value::Record(
            vec![(
                "a".into(),
                Value::Datum(uqa_core::DatumValue::new(17, 0, vec![5, b'x'])),
            )]
            .into(),
        )]);
        let fresh = PhysicalRow::from_values(vec![Value::Record(
            vec![("a".into(), Value::Bytes(b"x".to_vec()))].into(),
        )]);
        assert!(
            positional_keys_equal(schema.view(&old), &[0], schema.view(&fresh), &[0], None)
                .unwrap()
        );
        let bad = PhysicalRow::from_values(vec![Value::Record(
            vec![(
                "a".into(),
                Value::Datum(uqa_core::DatumValue::new(
                    1700,
                    0,
                    vec![2, 0, 0, 0, 3, b'x'],
                )),
            )]
            .into(),
        )]);
        assert!(
            positional_keys_equal(schema.view(&bad), &[0], schema.view(&bad), &[0], None)
                .unwrap_err()
                .to_string()
                .contains("compressed pglz data is corrupt")
        );
    }
}
