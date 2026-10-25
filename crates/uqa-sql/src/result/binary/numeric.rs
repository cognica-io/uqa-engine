//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Network-order base-10000 numeric digits preserve scale and special signs.

use super::{size_error, Encoder, SQLError};
use uqa_core::DecimalValue;

impl Encoder<'_, '_> {
    pub(super) fn numeric(&mut self, value: &DecimalValue) -> Result<(), SQLError> {
        let special: Option<u16> = if value.is_nan() {
            Some(0xc000)
        } else if value.is_positive_infinity() {
            Some(0xd000)
        } else if value.is_negative_infinity() {
            Some(0xf000)
        } else {
            None
        };
        if let Some(sign) = special {
            self.write(&[0; 4])?;
            self.write(&sign.to_be_bytes())?;
            // PostgreSQL's compact infinity header exposes scale 32; NaN exposes zero.
            return self.write(&(if value.is_infinite() { 32_u16 } else { 0 }).to_be_bytes());
        }
        let text = value.to_sql_string_with_control(&self.control)?;
        let text = text.strip_prefix('-').unwrap_or(&text);
        let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
        let integer_groups = integer.len().div_ceil(4);
        let groups = integer_groups + fraction.len().div_ceil(4);
        let padding = integer_groups * 4 - integer.len();
        let digit = |group: usize| -> u16 {
            let mut value = 0;
            for offset in 0..4 {
                let position = group * 4 + offset;
                let byte = if position < padding {
                    b'0'
                } else if position - padding < integer.len() {
                    integer.as_bytes()[position - padding]
                } else {
                    fraction
                        .as_bytes()
                        .get(position - padding - integer.len())
                        .copied()
                        .unwrap_or(b'0')
                };
                value = value * 10 + u16::from(byte - b'0');
            }
            value
        };
        let mut first = 0;
        while first < groups && digit(first) == 0 {
            self.control.check()?;
            first += 1;
        }
        let mut end = groups;
        while end > first && digit(end - 1) == 0 {
            self.control.check()?;
            end -= 1;
        }
        let weight = if first == end {
            0
        } else {
            integer_groups as i64 - first as i64 - 1
        };
        self.write(
            &u16::try_from(end - first)
                .map_err(|_| size_error())?
                .to_be_bytes(),
        )?;
        self.write(
            &i16::try_from(weight)
                .map_err(|_| size_error())?
                .to_be_bytes(),
        )?;
        self.write(
            &(if value.is_negative() && first != end {
                0x4000_u16
            } else {
                0
            })
            .to_be_bytes(),
        )?;
        self.write(
            &u16::try_from(fraction.len())
                .map_err(|_| size_error())?
                .to_be_bytes(),
        )?;
        for index in first..end {
            self.write(&digit(index).to_be_bytes())?;
        }
        Ok(())
    }
}
