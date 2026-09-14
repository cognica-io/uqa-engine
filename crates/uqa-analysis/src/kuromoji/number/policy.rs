//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese numeral symbols, independent morphology accounting and typed numeric limits.

use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::filters::stream::{token_units, JapaneseToken};
use crate::kuromoji::KuromojiLimits;
use crate::morphology::filter::{AllocatedStream, Work};
use crate::morphology::number::{Policy, Resource, Symbols as NumberSymbols};
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

pub(super) struct Symbols;
impl NumberSymbols for Symbols {
    fn digit(unit: u16) -> Option<u8> {
        Some(match unit {
            0x0030..=0x0039 => (unit - 0x0030) as u8,
            0xff10..=0xff19 => (unit - 0xff10) as u8,
            0x3007 => 0,
            0x4e00 => 1,
            0x4e8c => 2,
            0x4e09 => 3,
            0x56db => 4,
            0x4e94 => 5,
            0x516d => 6,
            0x4e03 => 7,
            0x516b => 8,
            0x4e5d => 9,
            _ => return None,
        })
    }
    fn exponent(unit: u16) -> usize {
        match unit {
            0x5341 => 1,
            0x767e => 2,
            0x5343 => 3,
            0x4e07 => 4,
            0x5104 => 8,
            0x5146 => 12,
            0x4eac => 16,
            0x5793 => 20,
            _ => 0,
        }
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

struct JapanesePolicy(KuromojiLimits);
impl<T: JapaneseToken> Policy<T> for JapanesePolicy {
    type Symbols = Symbols;
    fn maximum_output(&self) -> usize {
        self.0.max_output_utf16
    }
    fn check(&self, resource: Resource, required: usize) -> AnalysisResult<()> {
        let (name, maximum) = match resource {
            Resource::InputUnits => ("Kuromoji input UTF-16 units", self.0.max_input_utf16),
            Resource::OutputUnits => ("Kuromoji output UTF-16 units", self.0.max_output_utf16),
            Resource::NumericUnits => ("Kuromoji numeric units", self.0.max_output_utf16),
            Resource::Tokens => ("Kuromoji output tokens", self.0.max_tokens),
        };
        check_limit(name, required, maximum)?;
        Ok(())
    }
    fn invalid(&self, reason: &'static str) -> AnalysisError {
        invalid("Kuromoji number", reason).into()
    }
    fn token_units(&self, token: &T, term: usize, work: &mut Work<'_>) -> AnalysisResult<usize> {
        token_units(token, term, 0, work)
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

pub(in crate::kuromoji) fn filter<T: JapaneseToken>(
    input: AllocatedStream<T>,
    limits: KuromojiLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<AllocatedStream<T>> {
    crate::morphology::number::filter(input, JapanesePolicy(limits), poll)
}
