//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive and floating-point conversions for decimal values.

use super::DecimalValue;

mod exact;

impl DecimalValue {
    pub fn from_i64(value: i64) -> Self {
        Self::from_i64_with_control(value, &crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary numeric production")
            .into_uncontrolled()
            .expect("ordinary numeric value")
    }

    pub fn from_i128(value: i128) -> Option<Self> {
        Self::finite(value, 0)
    }

    pub fn from_bool(value: bool) -> Self {
        Self::from_i64(i64::from(value))
    }

    pub fn from_f64_lossy(value: f64) -> Option<Self> {
        Self::from_f64_lossy_with_control(value, &crate::memory::ProductionControl::uncontrolled())
            .ok()??
            .into_uncontrolled()
            .ok()
    }

    pub fn to_i64_trunc(&self) -> Option<i64> {
        self.to_i64_trunc_with_control(&crate::memory::ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }

    pub fn to_f64(&self) -> Option<f64> {
        self.to_f64_with_control(&crate::memory::ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }
}

/// Return the unsigned significand and binary exponent of a finite IEEE 754 value.
pub(super) fn finite_float_parts(value: f64) -> (u64, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mut significand = bits & ((1_u64 << 52) - 1);
    let power = if exponent == 0 {
        -1074
    } else {
        significand |= 1_u64 << 52;
        exponent - 1023 - 52
    };
    (significand, power)
}
