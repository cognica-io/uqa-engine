//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive and floating-point conversions for decimal values.

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use super::{pow10, DecimalRepr, DecimalValue};

impl DecimalValue {
    pub fn from_i64(value: i64) -> Self {
        Self::with_repr(DecimalRepr::Finite {
            coefficient: BigInt::from(value),
            scale: 0,
        })
    }

    pub fn from_i128(value: i128) -> Option<Self> {
        Self::finite(BigInt::from(value), 0)
    }

    pub fn from_bool(value: bool) -> Self {
        Self::from_i64(i64::from(value))
    }

    pub fn from_f64_lossy(value: f64) -> Option<Self> {
        if value.is_nan() {
            return Some(Self::nan());
        }
        if value == f64::INFINITY {
            return Some(Self::positive_infinity());
        }
        if value == f64::NEG_INFINITY {
            return Some(Self::negative_infinity());
        }
        let parsed = Self::parse(&value.to_string())?;
        if value != 0.0 && parsed.is_zero() {
            return None;
        }
        Some(parsed)
    }

    pub fn to_i64_trunc(&self) -> Option<i64> {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return None;
        };
        (coefficient / pow10(*scale)).to_i64()
    }

    pub fn to_f64(&self) -> Option<f64> {
        match self.repr() {
            DecimalRepr::NegativeInfinity => Some(f64::NEG_INFINITY),
            DecimalRepr::PositiveInfinity => Some(f64::INFINITY),
            DecimalRepr::NaN => Some(f64::NAN),
            DecimalRepr::Finite { .. } => {
                let value = self.to_sql_string().parse::<f64>().ok()?;
                value.is_finite().then_some(value)
            }
        }
    }
}
