//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact binary-float values for internal total ordering and equality keys.

use super::super::{coefficient, DecimalRepr, DecimalValue};
use crate::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

impl DecimalValue {
    /// Represent the stored binary value exactly. SQL casts retain their separately selected conversion rules.
    pub fn from_f64_exact(value: f64) -> Self {
        Self::from_f64_exact_with_control(value, &ProductionControl::uncontrolled())
            .expect("ordinary exact float conversion")
            .into_uncontrolled()
            .expect("ordinary exact decimal value")
    }

    /// Own all coefficient workspace before conversion. Every finite f64 fits the decimal carrier: at most 309 integer digits or 1,074 fractional digits.
    pub fn from_f64_exact_with_control(
        value: f64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        if !value.is_finite() {
            let memory = control.reserve(size_of::<DecimalRepr>())?;
            let repr = if value.is_nan() {
                DecimalRepr::NaN
            } else if value.is_sign_negative() {
                DecimalRepr::NegativeInfinity
            } else {
                DecimalRepr::PositiveInfinity
            };
            return control.finish(Self::with_repr(repr), memory);
        }
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        let mut significand = bits & ((1_u64 << 52) - 1);
        let mut power = if exponent == 0 {
            -1074
        } else {
            significand |= 1_u64 << 52;
            exponent - 1023 - 52
        };
        if significand == 0 {
            return Self::from_i64_with_control(0, control);
        }
        if power < 0 {
            let cancelled = significand.trailing_zeros().min(power.unsigned_abs());
            significand >>= cancelled;
            power += cancelled as i32;
        }
        let signed = if value.is_sign_negative() {
            -(significand as i64)
        } else {
            significand as i64
        };
        let mut coefficient = coefficient::from_i64(signed, control)?;
        let scale = if power < 0 { power.unsigned_abs() } else { 0 };
        if power != 0 {
            // m / 2^n = (m * 5^n) / 10^n; positive binary exponents multiply by 2^n.
            let factor = coefficient::power_of_small(
                if power < 0 { 5 } else { 2 },
                power.unsigned_abs(),
                control,
            )?;
            coefficient = coefficient::multiply(&coefficient, &factor, control)?;
        }
        let repr_memory = control.reserve(size_of::<DecimalRepr>())?;
        let (coefficient, memory) = coefficient.into_parts();
        control.finish(
            Self::with_repr(DecimalRepr::Finite { coefficient, scale }),
            control.combine(memory, repr_memory),
        )
    }
}

#[cfg(test)]
mod tests;
