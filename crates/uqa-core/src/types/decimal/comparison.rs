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
