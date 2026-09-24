//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact decimal equality and ordering.

use std::cmp::Ordering;

use num_bigint::BigInt;

use super::{align_coefficient, DecimalRepr, DecimalValue};

fn compare_finite(left: &BigInt, left_scale: u32, right: &BigInt, right_scale: u32) -> Ordering {
    let scale = left_scale.max(right_scale);
    align_coefficient(left, left_scale, scale).cmp(&align_coefficient(right, right_scale, scale))
}

impl PartialEq for DecimalValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DecimalValue {}

impl PartialOrd for DecimalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DecimalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        use DecimalRepr::{Finite, NaN, NegativeInfinity, PositiveInfinity};
        match (self.repr(), other.repr()) {
            (NaN, NaN)
            | (NegativeInfinity, NegativeInfinity)
            | (PositiveInfinity, PositiveInfinity) => Ordering::Equal,
            (NaN, _) => Ordering::Greater,
            (_, NaN) => Ordering::Less,
            (PositiveInfinity, _) => Ordering::Greater,
            (_, PositiveInfinity) | (NegativeInfinity, _) => Ordering::Less,
            (_, NegativeInfinity) => Ordering::Greater,
            (
                Finite {
                    coefficient: left,
                    scale: left_scale,
                },
                Finite {
                    coefficient: right,
                    scale: right_scale,
                },
            ) => compare_finite(left, *left_scale, right, *right_scale),
        }
    }
}

impl DecimalValue {
    /// Compare without allocating aligned coefficients. Formatting owns its radix workspace; comparison borrows the admitted digits and supplies fractional zeros without constructing padding.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &crate::memory::ProductionControl<'_>,
    ) -> Result<Ordering, crate::ValueRetentionError> {
        control.check()?;
        let (
            DecimalRepr::Finite {
                coefficient: left,
                scale: left_scale,
            },
            DecimalRepr::Finite {
                coefficient: right,
                scale: right_scale,
            },
        ) = (self.repr(), other.repr())
        else {
            return Ok(self.cmp(other));
        };
        if left_scale == right_scale {
            return Ok(left.cmp(right));
        }
        let signs = self.sign().cmp(&other.sign());
        if signs != Ordering::Equal || self.sign() == 0 {
            return Ok(signs);
        }
        let left = self.to_sql_string_with_control(control)?;
        let right = other.to_sql_string_with_control(control)?;
        let (left_integer, left_fraction) = unsigned_text_parts(&left);
        let (right_integer, right_fraction) = unsigned_text_parts(&right);
        let mut ordering = left_integer.len().cmp(&right_integer.len());
        if ordering == Ordering::Equal {
            for (left, right) in left_integer.bytes().zip(right_integer.bytes()) {
                control.check()?;
                ordering = left.cmp(&right);
                if ordering != Ordering::Equal {
                    break;
                }
            }
        }
        if ordering == Ordering::Equal {
            for index in 0..left_fraction.len().max(right_fraction.len()) {
                control.check()?;
                let left = left_fraction.as_bytes().get(index).copied().unwrap_or(b'0');
                let right = right_fraction
                    .as_bytes()
                    .get(index)
                    .copied()
                    .unwrap_or(b'0');
                ordering = left.cmp(&right);
                if ordering != Ordering::Equal {
                    break;
                }
            }
        }
        Ok(if self.sign() < 0 {
            ordering.reverse()
        } else {
            ordering
        })
    }
}

fn unsigned_text_parts(text: &str) -> (&str, &str) {
    let text = text.strip_prefix('-').unwrap_or(text);
    text.split_once('.').unwrap_or((text, ""))
}

#[cfg(test)]
mod tests;
