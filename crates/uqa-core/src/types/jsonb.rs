//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` `jsonb` structural equality, ordering, and canonical hash keys.

use std::cmp::Ordering;

mod equality;
mod key;
mod parser;
mod workspace;

pub use equality::{jsonb_equality_key, jsonb_equality_key_with_control, write_jsonb_equality_key};
pub use key::{write_jsonb_comparison_key, JsonbKeyError};
use parser::JsonbParser;

#[derive(Debug, PartialEq, Eq)]
struct JsonNumber {
    negative: bool,
    digits: Vec<u8>,
    power: i64,
}

impl JsonNumber {
    fn parse(
        text: &str,
        check: &mut impl FnMut() -> Result<(), JsonbKeyError>,
    ) -> Result<Option<Self>, JsonbKeyError> {
        check()?;
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |unsigned| (true, unsigned));
        let mut exponent_at = None;
        for (index, byte) in unsigned.bytes().enumerate() {
            if index.is_multiple_of(4096) {
                check()?;
            }
            if matches!(byte, b'e' | b'E') {
                exponent_at = Some(index);
                break;
            }
        }
        let (mantissa, exponent) = if let Some(index) = exponent_at {
            let Ok(exponent) = unsigned[index + 1..].parse::<i64>() else {
                return Ok(None);
            };
            (&unsigned[..index], exponent)
        } else {
            (unsigned, 0_i64)
        };
        let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut digits = Vec::new();
        for (index, digit) in integer.bytes().chain(fraction.bytes()).enumerate() {
            if index.is_multiple_of(4096) {
                check()?;
            }
            if digit != b'0' || !digits.is_empty() {
                digits.push(digit);
            }
        }
        if digits.is_empty() {
            digits.push(b'0');
            return Ok(Some(Self {
                negative: false,
                digits,
                power: 0,
            }));
        }
        let Ok(fraction_len) = i64::try_from(fraction.len()) else {
            return Ok(None);
        };
        let Some(mut power) = exponent.checked_sub(fraction_len) else {
            return Ok(None);
        };
        while digits.len() > 1 && digits.last() == Some(&b'0') {
            if digits.len().is_multiple_of(4096) {
                check()?;
            }
            digits.pop();
            let Some(next) = power.checked_add(1) else {
                return Ok(None);
            };
            power = next;
        }
        check()?;
        Ok(Some(Self {
            negative,
            digits,
            power,
        }))
    }

    fn cmp(&self, other: &Self) -> Ordering {
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = self.cmp_magnitude(other);
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => {}
        }
        let ordering = self.integer_digits().cmp(&other.integer_digits());
        if ordering != Ordering::Equal {
            return ordering;
        }
        let width = self.digits.len().max(other.digits.len());
        (0..width)
            .map(|index| {
                self.digits
                    .get(index)
                    .copied()
                    .unwrap_or(b'0')
                    .cmp(&other.digits.get(index).copied().unwrap_or(b'0'))
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    }

    fn is_zero(&self) -> bool {
        self.digits.as_slice() == b"0"
    }

    fn integer_digits(&self) -> i128 {
        i128::try_from(self.digits.len())
            .unwrap_or(i128::MAX)
            .saturating_add(i128::from(self.power))
    }
}

#[derive(Debug)]
enum JsonbValue {
    Null,
    String(String),
    Number(JsonNumber),
    Bool(bool),
    Array(Vec<JsonbValue>),
    Object(Vec<JsonbField>),
}

#[derive(Debug)]
struct JsonbField {
    name: String,
    value: JsonbValue,
    position: usize,
}

pub(super) fn compare_jsonb_text(left: &str, right: &str) -> Ordering {
    match (JsonbParser::parse(left), JsonbParser::parse(right)) {
        (Some(left), Some(right)) => compare_root(&left, &right),
        _ => left.cmp(right),
    }
}

pub(super) fn compare_jsonb_text_with_control(
    left: &str,
    right: &str,
    control: &crate::memory::ProductionControl<'_>,
) -> Result<Ordering, crate::ValueRetentionError> {
    let Some(budget) = control.budget() else {
        return Ok(compare_jsonb_text(left, right));
    };
    let mut left_key = crate::memory::BudgetedVec::new(budget);
    let mut right_key = crate::memory::BudgetedVec::new(budget);
    let left_valid = comparison_key(left, &mut left_key, control)?;
    let right_valid = comparison_key(right, &mut right_key, control)?;
    if left_valid && right_valid {
        crate::types::value::comparison_control::compare_bytes(&left_key, &right_key, control)
    } else {
        crate::types::value::comparison_control::compare_bytes(
            left.as_bytes(),
            right.as_bytes(),
            control,
        )
    }
}

fn comparison_key(
    text: &str,
    output: &mut crate::memory::BudgetedVec<u8>,
    control: &crate::memory::ProductionControl<'_>,
) -> Result<bool, crate::ValueRetentionError> {
    match key::write_with_control(text, output, control) {
        Ok(()) => Ok(true),
        Err(JsonbKeyError::InvalidJson) => Ok(false),
        Err(JsonbKeyError::Memory(error)) => Err(error.into()),
        Err(JsonbKeyError::Cancelled(error)) => Err(error.into()),
    }
}

fn compare_root(left: &JsonbValue, right: &JsonbValue) -> Ordering {
    match (left, right) {
        (JsonbValue::Array(left), right) if left.is_empty() && is_scalar(right) => Ordering::Less,
        (left, JsonbValue::Array(right)) if right.is_empty() && is_scalar(left) => {
            Ordering::Greater
        }
        _ => compare_value(left, right),
    }
}

fn is_scalar(value: &JsonbValue) -> bool {
    matches!(
        value,
        JsonbValue::Null | JsonbValue::Bool(_) | JsonbValue::Number(_) | JsonbValue::String(_)
    )
}

fn compare_value(left: &JsonbValue, right: &JsonbValue) -> Ordering {
    let ordering = type_rank(left).cmp(&type_rank(right));
    if ordering != Ordering::Equal {
        return ordering;
    }
    match (left, right) {
        (JsonbValue::String(left), JsonbValue::String(right)) => left.cmp(right),
        (JsonbValue::Number(left), JsonbValue::Number(right)) => left.cmp(right),
        (JsonbValue::Bool(left), JsonbValue::Bool(right)) => left.cmp(right),
        (JsonbValue::Array(left), JsonbValue::Array(right)) => {
            left.len().cmp(&right.len()).then_with(|| {
                compare_sequence(left.iter().zip(right), |(left, right)| {
                    compare_value(left, right)
                })
            })
        }
        (JsonbValue::Object(left), JsonbValue::Object(right)) => left
            .len()
            .cmp(&right.len())
            .then_with(|| compare_objects(left, right)),
        _ => Ordering::Equal,
    }
}

fn type_rank(value: &JsonbValue) -> u8 {
    match value {
        JsonbValue::Null => 0,
        JsonbValue::String(_) => 1,
        JsonbValue::Number(_) => 2,
        JsonbValue::Bool(_) => 3,
        JsonbValue::Array(_) => 4,
        JsonbValue::Object(_) => 5,
    }
}

fn compare_sequence<T>(
    values: impl IntoIterator<Item = T>,
    compare: impl Fn(T) -> Ordering,
) -> Ordering {
    values
        .into_iter()
        .map(compare)
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn compare_objects(left: &[JsonbField], right: &[JsonbField]) -> Ordering {
    let mut left = left.iter().collect::<Vec<_>>();
    let mut right = right.iter().collect::<Vec<_>>();
    left.sort_unstable_by(|left, right| jsonb_key_storage_order(&left.name, &right.name));
    right.sort_unstable_by(|left, right| jsonb_key_storage_order(&left.name, &right.name));
    compare_sequence(left.into_iter().zip(right), |(left, right)| {
        left.name
            .cmp(&right.name)
            .then_with(|| compare_value(&left.value, &right.value))
    })
}

fn jsonb_key_storage_order(left: &str, right: &str) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}
