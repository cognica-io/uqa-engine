//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` floating-point text formatting shared by SQL and graph values.

/// `PostgreSQL` `float8out` shortest-round-trip formatting: fixed notation while the decimal exponent is in `[-4, 15)`, scientific (`1e+15`, `1e-05`) otherwise, with `NaN` and `Infinity` spelled out.
#[must_use]
pub fn format_float_pg(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // `{:e}` prints the shortest round-trip mantissa in scientific
    // form (`-3.25e-2`); re-shape it into PostgreSQL conventions.
    let sci = format!("{f:e}");
    let Some((mantissa, exp)) = sci.split_once('e') else {
        return sci;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return sci;
    };
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };

    if (-4..15).contains(&exp) {
        if exp >= 0 {
            let Ok(int_len) = usize::try_from(exp + 1) else {
                return sci;
            };
            if digits.len() > int_len {
                format!("{sign}{}.{}", &digits[..int_len], &digits[int_len..])
            } else {
                let zeros = "0".repeat(int_len - digits.len());
                format!("{sign}{digits}{zeros}")
            }
        } else {
            let Ok(zero_count) = usize::try_from(-exp - 1) else {
                return sci;
            };
            let zeros = "0".repeat(zero_count);
            format!("{sign}0.{zeros}{digits}")
        }
    } else {
        let mantissa_text = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!("{sign}{mantissa_text}e{exp:+03}")
    }
}
