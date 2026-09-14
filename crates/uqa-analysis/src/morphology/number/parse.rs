//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reference prefix parsing, with malformed decimals distinct from resource errors.

use super::Symbols;
use crate::morphology::decimal::Context;
use crate::morphology::decimal::Decimal;
use crate::AnalysisError;
use std::marker::PhantomData;
use uqa_core::memory::BudgetedDeque;

enum ParseError {
    Malformed,
    Analysis(AnalysisError),
}

impl From<AnalysisError> for ParseError {
    fn from(error: AnalysisError) -> Self {
        Self::Analysis(error)
    }
}

type Result<T> = std::result::Result<T, ParseError>;

struct Parser<'a, C: Context, S: Symbols> {
    input: &'a [u16],
    offset: usize,
    context: &'a mut C,
    symbols: PhantomData<S>,
}

impl<C: Context, S: Symbols> Parser<'_, C, S> {
    fn basic(&mut self) -> Result<Option<Decimal>> {
        let start = self.offset;
        let mut count = 0;
        let mut dot = false;
        let mut scale = 0;
        while let Some(&unit) = self.input.get(self.offset) {
            self.context.tick()?;
            if S::digit(unit).is_some() {
                count += 1;
                self.context.check_digits(count)?;
                scale += usize::from(dot);
            } else if S::decimal_point(unit) {
                if dot {
                    return Err(ParseError::Malformed);
                }
                dot = true;
            } else if !S::separator(unit) {
                break;
            }
            self.offset += 1;
        }
        if count == 0 {
            return if dot {
                Err(ParseError::Malformed)
            } else {
                Ok(None)
            };
        }
        let mut digits = BudgetedDeque::new(self.context.budget());
        digits.reserve(count).map_err(AnalysisError::from)?;
        for unit in &self.input[start..self.offset] {
            self.context.tick()?;
            if let Some(value) = S::digit(*unit) {
                digits.push_back(value).map_err(AnalysisError::from)?;
            }
        }
        Ok(Some(Decimal::from_digits(digits, scale, self.context)?))
    }

    fn power(&mut self, large: bool) -> usize {
        let power = self
            .input
            .get(self.offset)
            .map_or(0, |unit| S::exponent(*unit));
        if power > 0 && S::large_power(power) == large {
            self.offset += 1;
            power
        } else {
            0
        }
    }

    fn pair(&mut self, large: bool) -> Result<Option<Decimal>> {
        let first = if large { self.medium()? } else { self.basic()? };
        let power = self.power(large);
        Ok(match (first, power) {
            (first, 0) => first,
            (None, power) => Some(Decimal::power(power, self.context)?),
            (Some(first), power) => Some(first.multiply_power(power, self.context)?),
        })
    }

    fn medium(&mut self) -> Result<Option<Decimal>> {
        self.sum(false)
    }

    fn sum(&mut self, large: bool) -> Result<Option<Decimal>> {
        let mut sum = self.pair(large)?;
        if sum.is_none() {
            return Ok(None);
        }
        while let Some(next) = self.pair(large)? {
            self.context.tick()?;
            sum = Some(sum.take().expect("nonempty sum").add(&next, self.context)?);
        }
        Ok(sum)
    }
}

pub(crate) fn parse<S: Symbols>(
    input: &[u16],
    context: &mut impl Context,
) -> crate::AnalysisResult<Option<Decimal>> {
    match (Parser {
        input,
        offset: 0,
        context,
        symbols: PhantomData::<S>,
    })
    .sum(true)
    {
        Ok(value) => Ok(value),
        Err(ParseError::Malformed) => Ok(None),
        Err(ParseError::Analysis(error)) => Err(error),
    }
}
