//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column bindings for allocation-free projected group lookup.

use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_sql::expr::RowLookup;

pub(super) enum ProjectedGroupColumn {
    Position(usize),
}

impl ProjectedGroupColumn {
    pub(super) fn compile(
        expressions: &[ScalarExpr],
        input_schema: &[String],
    ) -> Option<Vec<Self>> {
        expressions
            .iter()
            .map(|expression| {
                super::projected_input::column_slot(expression, input_schema).map(Self::Position)
            })
            .collect()
    }

    #[inline]
    pub(super) fn value<'row>(&self, row: &'row dyn RowLookup) -> Option<&'row Value> {
        match self {
            Self::Position(index) => row.positional_column(*index),
        }
    }
}

#[inline]
pub(super) fn group_fingerprint(
    columns: &[ProjectedGroupColumn],
    row: &dyn RowLookup,
    null: &Value,
) -> u64 {
    // This is only a bounded lookup accelerator. Collisions are harmless:
    // `group_matches` verifies the complete key before a group is reused.
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    for column in columns {
        fingerprint_value(&mut fingerprint, column.value(row).unwrap_or(null));
    }
    fingerprint
}

#[inline]
pub(super) fn group_matches(
    columns: &[ProjectedGroupColumn],
    key: &[Value],
    row: &dyn RowLookup,
    null: &Value,
) -> bool {
    key.len() == columns.len()
        && key
            .iter()
            .zip(columns)
            .all(|(stored, column)| stored == column.value(row).unwrap_or(null))
}

pub(super) fn group_key(
    columns: &[ProjectedGroupColumn],
    row: &dyn RowLookup,
    null: &Value,
) -> Vec<Value> {
    columns
        .iter()
        .map(|column| column.value(row).unwrap_or(null).clone())
        .collect()
}

#[inline]
fn fingerprint_value(fingerprint: &mut u64, value: &Value) {
    match value {
        Value::Null => mix_fingerprint(fingerprint, 0),
        // Value ordering coerces all numeric variants. A shared token keeps
        // the fingerprint compatible with that equality contract.
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_) => {
            mix_fingerprint(fingerprint, 1);
        }
        Value::Str(value) => fingerprint_bytes(fingerprint, 2, value.as_bytes()),
        Value::Bytes(value) => fingerprint_bytes(fingerprint, 3, value),
        Value::Temporal(_) => mix_fingerprint(fingerprint, 4),
        Value::List(values) => {
            mix_fingerprint(fingerprint, 5);
            mix_fingerprint(fingerprint, values.len() as u64);
            if let Some(first) = values.first() {
                fingerprint_value(fingerprint, first);
            }
            if values.len() > 1 {
                fingerprint_value(fingerprint, &values[values.len() - 1]);
            }
        }
        Value::Map(values) => {
            mix_fingerprint(fingerprint, 6);
            mix_fingerprint(fingerprint, values.len() as u64);
            if let Some((key, value)) = values.first_key_value() {
                fingerprint_bytes(fingerprint, 7, key.as_bytes());
                fingerprint_value(fingerprint, value);
            }
            if values.len() > 1 {
                if let Some((key, value)) = values.last_key_value() {
                    fingerprint_bytes(fingerprint, 8, key.as_bytes());
                    fingerprint_value(fingerprint, value);
                }
            }
        }
    }
}

#[inline]
fn fingerprint_bytes(fingerprint: &mut u64, tag: u64, bytes: &[u8]) {
    mix_fingerprint(fingerprint, tag);
    mix_fingerprint(fingerprint, bytes.len() as u64);
    mix_fingerprint(fingerprint, sampled_word(bytes.iter().take(8).copied()));
    if bytes.len() > 8 {
        mix_fingerprint(
            fingerprint,
            sampled_word(bytes.iter().rev().take(8).copied()),
        );
    }
}

#[inline]
fn sampled_word(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.enumerate().fold(0u64, |word, (index, byte)| {
        word | (u64::from(byte) << (index * 8))
    })
}

#[inline]
fn mix_fingerprint(fingerprint: &mut u64, word: u64) {
    *fingerprint ^= word
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(*fingerprint << 6)
        .wrapping_add(*fingerprint >> 2);
}
