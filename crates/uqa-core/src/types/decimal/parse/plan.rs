//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free syntax and precision validation derives the expanded coefficient shape.

use super::super::{DecimalRepr, MAX_FRACTIONAL_DIGITS, MAX_INTEGER_DIGITS};
use crate::ValueRetentionError;

pub(super) enum Plan<'a> {
    Special(DecimalRepr),
    Finite(FinitePlan<'a>),
}

pub(super) struct FinitePlan<'a> {
    pub(super) significand: &'a str,
    pub(super) negative: bool,
    pub(super) trailing_zeros: usize,
    pub(super) digit_count: usize,
    pub(super) scale: u32,
}

impl<'a> Plan<'a> {
    pub(super) fn parse(
        input: &'a str,
        check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        let input = trim(input, check)?;
        if let Some(special) = special(input) {
            return Ok(Some(Self::Special(special)));
        }
        let negative = input.starts_with('-');
        let unsigned = input.strip_prefix(['-', '+']).unwrap_or(input);
        let mut point = None;
        let mut digit_count = 0_usize;
        let mut leading_zeros = 0_usize;
        let mut first_nonzero = None;
        let mut end = unsigned.len();
        let mut exponent = 0;
        for (index, byte) in unsigned.bytes().enumerate() {
            if index.is_multiple_of(4096) {
                check()?;
            }
            match byte {
                b'0'..=b'9' => {
                    digit_count += 1;
                    if first_nonzero.is_none() {
                        if byte == b'0' {
                            leading_zeros += 1;
                        } else {
                            first_nonzero = Some(index);
                        }
                    }
                }
                b'.' if point.is_none() => point = Some(index),
                b'e' | b'E' => {
                    end = index;
                    let Some(value) = parse_exponent(&unsigned[index + 1..], check)? else {
                        return Ok(None);
                    };
                    exponent = value;
                    break;
                }
                _ => return Ok(None),
            }
        }
        if digit_count == 0 {
            return Ok(None);
        }
        let fractional = point.map_or(0, |point| end - point - 1);
        let Some(scale) = i64::try_from(fractional)
            .ok()
            .and_then(|fractional| fractional.checked_sub(i64::from(exponent)))
        else {
            return Ok(None);
        };
        let scale = scale.max(0);
        let Ok(scale) = u32::try_from(scale) else {
            return Ok(None);
        };
        if scale > MAX_FRACTIONAL_DIGITS {
            return Ok(None);
        }
        let Some(first_nonzero) = first_nonzero else {
            return Ok(Some(Self::Finite(FinitePlan {
                significand: "",
                negative: false,
                trailing_zeros: 0,
                digit_count: 0,
                scale,
            })));
        };
        let trailing_zeros = usize::try_from(i64::from(exponent))
            .unwrap_or(0)
            .saturating_sub(fractional);
        let Some(digit_count) = (digit_count - leading_zeros).checked_add(trailing_zeros) else {
            return Ok(None);
        };
        if digit_count.saturating_sub(scale as usize) > MAX_INTEGER_DIGITS {
            return Ok(None);
        }
        Ok(Some(Self::Finite(FinitePlan {
            significand: &unsigned[first_nonzero..end],
            negative,
            trailing_zeros,
            digit_count,
            scale,
        })))
    }
}

fn special(input: &str) -> Option<DecimalRepr> {
    if input.eq_ignore_ascii_case("nan") {
        Some(DecimalRepr::NaN)
    } else if ["infinity", "+infinity", "inf", "+inf"]
        .iter()
        .any(|special| input.eq_ignore_ascii_case(special))
    {
        Some(DecimalRepr::PositiveInfinity)
    } else if ["-infinity", "-inf"]
        .iter()
        .any(|special| input.eq_ignore_ascii_case(special))
    {
        Some(DecimalRepr::NegativeInfinity)
    } else {
        None
    }
}

fn parse_exponent(
    input: &str,
    check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
) -> Result<Option<i32>, ValueRetentionError> {
    let negative = input.starts_with('-');
    let unsigned = input.strip_prefix(['-', '+']).unwrap_or(input);
    if unsigned.is_empty() {
        return Ok(None);
    }
    let mut value = 0_i32;
    for (index, byte) in unsigned.bytes().enumerate() {
        if index.is_multiple_of(4096) {
            check()?;
        }
        if !byte.is_ascii_digit() {
            return Ok(None);
        }
        let Some(next) = value.checked_mul(10).and_then(|value| {
            if negative {
                value.checked_sub(i32::from(byte - b'0'))
            } else {
                value.checked_add(i32::from(byte - b'0'))
            }
        }) else {
            return Ok(None);
        };
        value = next;
    }
    Ok(Some(value))
}

fn trim<'a>(
    input: &'a str,
    check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
) -> Result<&'a str, ValueRetentionError> {
    let mut start = input.len();
    for (count, (index, character)) in input.char_indices().enumerate() {
        if count.is_multiple_of(4096) {
            check()?;
        }
        if !character.is_whitespace() {
            start = index;
            break;
        }
    }
    let input = &input[start..];
    let mut end = 0;
    for (count, (index, character)) in input.char_indices().rev().enumerate() {
        if count.is_multiple_of(4096) {
            check()?;
        }
        if !character.is_whitespace() {
            end = index + character.len_utf8();
            break;
        }
    }
    Ok(&input[..end])
}
