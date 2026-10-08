//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free direct index for simple positional equality keys.

use crate::RowSchemaExecution;
use std::collections::HashMap;
use std::hash::BuildHasher;

use smallvec::SmallVec;
use uqa_core::Value;

use crate::distinct::hash_canonical_row;
use crate::{ExecResult, PhysicalRow, RowSchema};

use super::row_store::HybridRowStore;

/// Allocation-free in-memory index for simple positional equality keys.
///
/// Only the canonical hash and build-row position are retained. The key itself stays in the original [`PhysicalRow`]; hash collisions are resolved by comparing the mapped source slots. If the index exceeds its budget, its state is discarded and the caller rebuilds the spill-capable encoded index from the row store.
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

    pub(super) fn insert(&mut self, hash: u64, row_index: u64) -> ExecResult<()> {
        if self.overflowed {
            return Ok(());
        }

        // Account for the hash, inline first row index, control bytes, and
        // allocator/table slack. Duplicate-key indices need only one u64.
        let record_bytes = if self.buckets.contains_key(&hash) {
            8
        } else {
            64
        };
        let fits = self
            .memory_bytes
            .checked_add(record_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if !fits {
            self.buckets.clear();
            self.memory_bytes = 0;
            self.overflowed = true;
            return Ok(());
        }

        self.buckets.entry(hash).or_default().push(row_index);
        self.memory_bytes += record_bytes;
        Ok(())
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
                    if positional_keys_equal(schema, left, positions, schema, right, positions)? {
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
    schema: &RowSchema,
    row: &PhysicalRow,
    positions: &[usize],
) -> ExecResult<Option<u64>> {
    let view = schema.view(row);
    if positions.iter().any(|position| {
        view.value_at(*position)
            .is_none_or(|value| matches!(value, Value::Null))
    }) {
        return Ok(None);
    }
    hash_canonical_row(
        build_hasher,
        positions.iter().map(|position| view.value_at(*position)),
    )
    .map(Some)
}

fn positional_keys_equal(
    left_schema: &RowSchema,
    left_row: &PhysicalRow,
    left_positions: &[usize],
    right_schema: &RowSchema,
    right_row: &PhysicalRow,
    right_positions: &[usize],
) -> ExecResult<bool> {
    if left_positions.len() != right_positions.len() {
        return Ok(false);
    }
    let left = left_schema.view(left_row);
    let right = right_schema.view(right_row);
    for (left_position, right_position) in left_positions.iter().zip(right_positions) {
        let (Some(left), Some(right)) = (
            left.value_at(*left_position),
            right.value_at(*right_position),
        ) else {
            return Ok(false);
        };
        if uqa_sql::expr::values_equal_nullable_with_control(
            left,
            right,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )? != Some(true)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn direct_unique_match(
    index: &DirectHashIndex,
    build_rows: &HybridRowStore,
    build_positions: &[usize],
    probe_schema: &RowSchema,
    probe_row: &PhysicalRow,
    probe_positions: &[usize],
) -> ExecResult<Option<u64>> {
    let Some(hash) = positional_key_hash(index.hasher(), probe_schema, probe_row, probe_positions)?
    else {
        return Ok(None);
    };
    for row_index in index.candidates(hash) {
        if let Some(build_row) = build_rows.memory_row(*row_index) {
            if positional_keys_equal(
                &build_rows.schema,
                build_row,
                build_positions,
                probe_schema,
                probe_row,
                probe_positions,
            )? {
                return Ok(Some(*row_index));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_join_datum_keys_use_sql_equality_and_preserve_errors() {
        let schema = RowSchema::new(vec!["key".into()]);
        let old = PhysicalRow::from_values(vec![Value::Record(vec![(
            "a".into(),
            Value::Datum(uqa_core::DatumValue::new(17, 0, vec![5, b'x'])),
        )])]);
        let fresh = PhysicalRow::from_values(vec![Value::Record(vec![(
            "a".into(),
            Value::Bytes(b"x".to_vec()),
        )])]);
        assert!(positional_keys_equal(&schema, &old, &[0], &schema, &fresh, &[0]).unwrap());
        let bad = PhysicalRow::from_values(vec![Value::Record(vec![(
            "a".into(),
            Value::Datum(uqa_core::DatumValue::new(
                1700,
                0,
                vec![2, 0, 0, 0, 3, b'x'],
            )),
        )])]);
        assert!(
            positional_keys_equal(&schema, &bad, &[0], &schema, &bad, &[0])
                .unwrap_err()
                .to_string()
                .contains("compressed pglz data is corrupt")
        );
    }
}
