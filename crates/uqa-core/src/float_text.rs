//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` floating-point text formatting shared by SQL and graph values.

use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// `PostgreSQL` `float8out` shortest-round-trip formatting: fixed notation while the decimal exponent is in `[-4, 15)`, scientific (`1e+15`, `1e-05`) otherwise, with `NaN` and `Infinity` spelled out.
#[must_use]
pub fn format_float_pg(f: f64) -> String {
    format_float_pg_with_control(f, &ProductionControl::uncontrolled())
        .expect("ordinary float formatting")
        .into_uncontrolled()
        .expect("ordinary float text")
}

pub fn format_float_pg_with_control(
    f: f64,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if f.is_nan() {
        return control.copy_text("NaN");
    }
    if f.is_infinite() {
        return control.copy_text(if f > 0.0 { "Infinity" } else { "-Infinity" });
    }
    let sci = control.format(format_args!("{f:e}"))?;
    let Some((mantissa, exp)) = sci.split_once('e') else {
        return Ok(sci);
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return Ok(sci);
    };
    let mut digits = ProductionString::new(*control);
    for character in mantissa.chars().filter(char::is_ascii_digit) {
        digits.push(character)?;
    }
    let sign = if mantissa.starts_with('-') { "-" } else { "" };
    let mut output = ProductionString::new(*control);
    output.push_str(sign)?;
    if (-4..15).contains(&exp) {
        if exp >= 0 {
            let Ok(int_len) = usize::try_from(exp + 1) else {
                return Ok(sci);
            };
            if digits.len() > int_len {
                output.push_str(&digits[..int_len])?;
                output.push('.')?;
                output.push_str(&digits[int_len..])?;
            } else {
                output.push_str(&digits)?;
                for _ in digits.len()..int_len {
                    output.push('0')?;
                }
            }
        } else {
            let Ok(zero_count) = usize::try_from(-exp - 1) else {
                return Ok(sci);
            };
            output.push_str("0.")?;
            for _ in 0..zero_count {
                output.push('0')?;
            }
            output.push_str(&digits)?;
        }
    } else {
        output.push_str(&digits[..1])?;
        if digits.len() > 1 {
            output.push('.')?;
            output.push_str(&digits[1..])?;
        }
        output.push_str(&control.format(format_args!("e{exp:+03}"))?)?;
    }
    output.finish()
}

#[cfg(test)]
mod tests;
