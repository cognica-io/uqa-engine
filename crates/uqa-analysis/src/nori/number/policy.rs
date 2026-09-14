//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean numeral grammar, attribute accounting and diagnostics for common number mechanics.

use crate::morphology::filter::{AllocatedStream, Work};
use crate::morphology::number::{Policy, Resource, Symbols as NumberSymbols};
use crate::nori::error::{check_limit, invalid};
use crate::nori::filters::stream::FilterToken;
use crate::nori::NoriLimits;
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

pub(super) struct Symbols;
impl NumberSymbols for Symbols {
    fn digit(unit: u16) -> Option<u8> {
        super::digit(unit)
    }
    fn exponent(unit: u16) -> usize {
        super::exponent(unit)
    }
    fn large_power(power: usize) -> bool {
        power > 3
    }
    fn decimal_point(unit: u16) -> bool {
        matches!(unit, 0x002e | 0xff0e)
    }
    fn separator(unit: u16) -> bool {
        matches!(unit, 0x002c | 0xff0c)
    }
}

struct KoreanPolicy(NoriLimits);
impl<T: FilterToken> Policy<T> for KoreanPolicy {
    type Symbols = Symbols;
    fn maximum_output(&self) -> usize {
        self.0.max_output_utf16
    }
    fn check(&self, resource: Resource, required: usize) -> AnalysisResult<()> {
        let (name, maximum) = match resource {
            Resource::InputUnits => ("Nori input UTF-16 units", self.0.max_input_utf16),
            Resource::OutputUnits => ("Nori output UTF-16 units", self.0.max_output_utf16),
            Resource::NumericUnits => ("Nori numeric units", self.0.max_output_utf16),
            Resource::Tokens => ("Nori output tokens", self.0.max_tokens),
        };
        check_limit(name, required, maximum)?;
        Ok(())
    }
    fn invalid(&self, reason: &'static str) -> AnalysisError {
        invalid("Nori number", reason).into()
    }
    fn token_units(
        &self,
        token: &T,
        term_units: usize,
        work: &mut Work<'_>,
    ) -> AnalysisResult<usize> {
        crate::nori::filters::token_units(token, term_units, work)
    }
    fn normalize(
        &self,
        input: &[u16],
        maximum: usize,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Vec<u16>>> {
        super::normalize_budgeted(input, maximum, budget, work)
    }
}

pub(in crate::nori) fn filter<T: FilterToken>(
    input: AllocatedStream<T>,
    limits: NoriLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<AllocatedStream<T>> {
    crate::morphology::number::filter(input, KoreanPolicy(limits), poll)
}
