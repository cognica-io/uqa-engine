//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact arithmetic, division-scale selection, and decimal quantization.

use super::DecimalValue;

impl DecimalValue {
    pub fn is_integral(&self) -> bool {
        self.is_integral_with_control(&crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal integrality")
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        self.checked_add_with_control(rhs, &crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal arithmetic")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        self.checked_sub_with_control(rhs, &crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal arithmetic")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        self.checked_mul_with_control(rhs, &crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal arithmetic")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        self.checked_div_postgres(rhs)
    }

    /// Divide at an explicit display scale with `PostgreSQL` numeric midpoint rounding. This is used by algorithms whose result scale is selected by the caller rather than by `select_div_scale()`.
    pub fn checked_div_to_scale(&self, rhs: &Self, result_scale: u32) -> Option<Self> {
        self.checked_div_to_scale_with_control(
            rhs,
            result_scale,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary decimal arithmetic")
        .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    /// Divide using `PostgreSQL`'s `select_div_scale()` rule: at least sixteen significant digits, never less scale than either finite operand, and midpoint rounding away from zero.
    pub fn checked_div_postgres(&self, rhs: &Self) -> Option<Self> {
        self.checked_div_postgres_with_control(
            rhs,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary decimal arithmetic")
        .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        self.checked_rem_with_control(rhs, &crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal arithmetic")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn abs(&self) -> Self {
        self.abs_with_control(&crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal absolute value")
            .into_uncontrolled()
            .expect("ordinary decimal owner")
    }

    pub fn ceil(&self) -> Self {
        self.integral_round(IntegralRounding::Ceil)
    }

    pub fn floor(&self) -> Self {
        self.integral_round(IntegralRounding::Floor)
    }

    pub fn trunc(&self) -> Self {
        self.integral_round(IntegralRounding::Trunc)
    }

    pub fn round_dp(&self, scale: u32) -> Self {
        i32::try_from(scale)
            .ok()
            .and_then(|scale| self.round_to_scale(scale))
            .unwrap_or_else(|| self.clone())
    }

    pub fn round_to_scale(&self, scale: i32) -> Option<Self> {
        self.quantize(scale, true)
    }

    pub fn trunc_to_scale(&self, scale: i32) -> Option<Self> {
        self.quantize(scale, false)
    }

    pub fn fits_precision(&self, precision: u32, scale: i32) -> bool {
        self.fits_precision_with_control(
            precision,
            scale,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary numeric precision")
    }

    fn integral_round(&self, rounding: IntegralRounding) -> Self {
        self.integral_round_with_control(
            rounding,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary decimal integral rounding")
        .into_uncontrolled()
        .expect("ordinary decimal owner")
    }

    fn quantize(&self, target_scale: i32, round: bool) -> Option<Self> {
        self.quantize_with_control(
            target_scale,
            round,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .ok()??
        .into_uncontrolled()
        .ok()
    }
}

#[derive(Clone, Copy)]
pub(super) enum IntegralRounding {
    Ceil,
    Floor,
    Trunc,
}
