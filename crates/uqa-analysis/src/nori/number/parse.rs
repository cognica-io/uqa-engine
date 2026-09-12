//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reference prefix parsing, with malformed decimals distinct from resource errors.

use super::decimal::Decimal;
use super::{digit, exponent, Context};
use crate::AnalysisError;

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

struct Parser<'a, 'b, 'c> {
    input: &'a [u16],
    offset: usize,
    context: &'a mut Context<'b, 'c>,
}

impl Parser<'_, '_, '_> {
    fn basic(&mut self) -> Result<Option<Decimal>> {
        let mut digits = Vec::new();
        let mut dot = false;
        let mut scale = 0;
        while let Some(&unit) = self.input.get(self.offset) {
            self.context.work.tick()?;
            if let Some(value) = digit(unit) {
                self.context.check_digits(digits.len() + 1)?;
                digits
                    .try_reserve(1)
                    .map_err(crate::nori::DictionaryError::from)
                    .map_err(AnalysisError::from)?;
                digits.push(value);
                scale += usize::from(dot);
            } else if matches!(unit, 0x002e | 0xff0e) {
                if dot {
                    return Err(ParseError::Malformed);
                }
                dot = true;
            } else if !matches!(unit, 0x002c | 0xff0c) {
                break;
            }
            self.offset += 1;
        }
        if digits.is_empty() {
            return if dot {
                Err(ParseError::Malformed)
            } else {
                Ok(None)
            };
        }
        Ok(Some(Decimal::from_digits(digits, scale, self.context)?))
    }

    fn power(&mut self, large: bool) -> usize {
        let power = self
            .input
            .get(self.offset)
            .map_or(0, |unit| exponent(*unit));
        if power > 0 && (power > 3) == large {
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
            self.context.work.tick()?;
            sum = Some(sum.take().expect("nonempty sum").add(&next, self.context)?);
        }
        Ok(sum)
    }
}

pub(super) fn parse(
    input: &[u16],
    context: &mut Context<'_, '_>,
) -> crate::AnalysisResult<Option<Decimal>> {
    match (Parser {
        input,
        offset: 0,
        context,
    })
    .sum(true)
    {
        Ok(value) => Ok(value),
        Err(ParseError::Malformed) => Ok(None),
        Err(ParseError::Analysis(error)) => Err(error),
    }
}
