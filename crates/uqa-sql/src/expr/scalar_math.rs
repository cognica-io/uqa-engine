//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mathematical, padding, formatting, encoding, and split built-ins.

use super::conversion::{float1_with_control, to_f64_with_control, to_i64_with_control};
use super::{float_to_i64_trunc, out_of_range, DecimalValue, Result, SQLError, Value};
use uqa_core::memory::{Produced, ProductionControl};

mod text;

pub(super) fn eval_math_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if name == "format" {
        return Some(text::ordinary_format(args));
    }
    if name == "random" {
        // Lightweight pseudo-random value derived from system time.
        use std::time::{SystemTime, UNIX_EPOCH};
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(0) as f64;
        return Some(Ok(Value::Float((time.sin().abs() * 1.0e9).fract())));
    }
    eval_math_functions_with_control(name, args, &ProductionControl::uncontrolled()).map(|result| {
        result?
            .into_uncontrolled()
            .map_err(|_| SQLError::Internal("ordinary math owner".into()))
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
pub(super) fn eval_math_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    const NAMES: &[&str] = &[
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "atan2",
        "sinh",
        "cosh",
        "tanh",
        "exp",
        "ln",
        "log",
        "log10",
        "log2",
        "cbrt",
        "gamma",
        "lgamma",
        "crc32",
        "crc32c",
        "sign",
        "trunc",
        "pi",
        "degrees",
        "radians",
        "width_bucket",
        "lpad",
        "rpad",
        "repeat",
        "translate",
        "overlay",
        "md5",
        "encode",
        "decode",
        "split_part",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Produced<Value>> {
        control.check()?;
        if matches!(
            name,
            "lpad"
                | "rpad"
                | "repeat"
                | "translate"
                | "overlay"
                | "md5"
                | "encode"
                | "decode"
                | "split_part"
        ) {
            return text::eval(name, args, control);
        }
        if matches!(name, "log" | "log10") {
            return logarithm(args, control);
        }
        if name == "trunc" {
            return truncate(args, control);
        }
        let scalar = (|| -> Result<Value> {
            match name {
                // Trig / math
                "sin" => float1_with_control(args, "sin", f64::sin, control),
                "cos" => float1_with_control(args, "cos", f64::cos, control),
                "tan" => float1_with_control(args, "tan", f64::tan, control),
                "asin" => float1_with_control(args, "asin", f64::asin, control),
                "acos" => float1_with_control(args, "acos", f64::acos, control),
                "atan" => float1_with_control(args, "atan", f64::atan, control),
                "atan2" => {
                    if args.len() != 2 {
                        return Err(SQLError::TypeMismatch("atan2 takes 2 args".into()));
                    }
                    if args.iter().any(|arg| matches!(arg, Value::Null)) {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Float(
                        to_f64_with_control(&args[0], control)?
                            .atan2(to_f64_with_control(&args[1], control)?),
                    ))
                }
                "sinh" => float1_with_control(args, "sinh", f64::sinh, control),
                "cosh" => float1_with_control(args, "cosh", f64::cosh, control),
                "tanh" => float1_with_control(args, "tanh", f64::tanh, control),
                "exp" => float1_with_control(args, "exp", f64::exp, control),
                "ln" => float1_with_control(args, "ln", f64::ln, control),
                "log2" => float1_with_control(args, "log2", f64::log2, control),
                // Route cbrt through exp(ln(x)/3): this reproduces glibc's
                // last-ulp behavior (`cbrt(27)` = 3.0000000000000004), which is
                // what PostgreSQL emits on Linux builds; platform `cbrt` on
                // macOS is correctly rounded and would diverge.
                "cbrt" => float1_with_control(
                    args,
                    "cbrt",
                    |x| {
                        if x == 0.0 {
                            0.0
                        } else {
                            x.signum() * (x.abs().ln() / 3.0).exp()
                        }
                    },
                    control,
                ),
                "gamma" => gamma(args, control),
                "lgamma" => lgamma(args, control),
                "crc32" | "crc32c" => {
                    if args.len() != 1 {
                        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
                    }
                    match &args[0] {
                        Value::Bytes(bytes) => {
                            let checksum = if name == "crc32" {
                                let mut hasher = crc32fast::Hasher::new();
                                for chunk in bytes.chunks(4096) {
                                    control.check()?;
                                    hasher.update(chunk);
                                }
                                hasher.finalize()
                            } else {
                                crc32c(bytes, control)?
                            };
                            Ok(Value::Int(i64::from(checksum)))
                        }
                        Value::Null => Ok(Value::Null),
                        other => Err(SQLError::TypeMismatch(format!(
                            "{name}: expected bytea, got {other:?}"
                        ))),
                    }
                }
                "sign" => {
                    if args.len() != 1 {
                        return Err(SQLError::TypeMismatch("sign takes 1 arg".into()));
                    }
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Int(match to_f64_with_control(&args[0], control)? {
                        v if v > 0.0 => 1,
                        v if v < 0.0 => -1,
                        _ => 0,
                    }))
                }
                "pi" => Ok(Value::Float(std::f64::consts::PI)),
                "degrees" => float1_with_control(args, "degrees", f64::to_degrees, control),
                "radians" => float1_with_control(args, "radians", f64::to_radians, control),
                "width_bucket" => {
                    if args.len() != 4 {
                        return Err(SQLError::TypeMismatch("width_bucket takes 4 args".into()));
                    }
                    let operand = to_f64_with_control(&args[0], control)?;
                    let low = to_f64_with_control(&args[1], control)?;
                    let high = to_f64_with_control(&args[2], control)?;
                    let count = to_i64_with_control(&args[3], control)?;
                    if count <= 0
                        || !low.is_finite()
                        || !high.is_finite()
                        || operand.is_nan()
                        || low == high
                    {
                        return Err(SQLError::TypeMismatch(
                    "width_bucket requires finite bounds, a non-NaN operand, a positive bucket count, and a non-empty range".into(),
                ));
                    }
                    let overflow_bucket =
                        count.checked_add(1).ok_or_else(|| out_of_range("bigint"))?;
                    if low < high {
                        if operand < low {
                            return Ok(Value::Int(0));
                        }
                        if operand >= high {
                            return Ok(Value::Int(overflow_bucket));
                        }
                        let width = (high - low) / count as f64;
                        let bucket = float_to_i64_trunc(((operand - low) / width).floor())?
                            .checked_add(1)
                            .ok_or_else(|| out_of_range("bigint"))?;
                        Ok(Value::Int(bucket))
                    } else {
                        if operand > low {
                            return Ok(Value::Int(0));
                        }
                        if operand <= high {
                            return Ok(Value::Int(overflow_bucket));
                        }
                        let width = (low - high) / count as f64;
                        let bucket = float_to_i64_trunc(((low - operand) / width).floor())?
                            .checked_add(1)
                            .ok_or_else(|| out_of_range("bigint"))?;
                        Ok(Value::Int(bucket))
                    }
                }
                _ => unreachable!("function family membership was checked before dispatch"),
            }
        })()?;
        plain(scalar, control)
    })())
}

fn plain(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn decimal(
    value: Produced<DecimalValue>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Decimal(value), memory)?)
}

fn logarithm(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    match args.len() {
        1 => plain(
            float1_with_control(args, "log", f64::log10, control)?,
            control,
        ),
        2 => {
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return plain(Value::Null, control);
            }
            let base = to_f64_with_control(&args[0], control)?;
            let value = to_f64_with_control(&args[1], control)?;
            let result = value.log(base);
            if args.iter().any(|arg| matches!(arg, Value::Float(_))) {
                return plain(Value::Float(result), control);
            }
            // Preserve the numeric overload's existing 16-digit rendering before decimal parsing.
            let text = control.format(format_args!("{result:.16}"))?;
            match DecimalValue::parse_with_control(&text, control)? {
                Some(value) => decimal(value, control),
                None => plain(Value::Float(result), control),
            }
        }
        _ => Err(SQLError::TypeMismatch("log takes 1 or 2 args".into())),
    }
}

fn truncate(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    match args.len() {
        1 => match &args[0] {
            Value::Int(value) => plain(Value::Int(*value), control),
            Value::Float(value) => plain(Value::Float(value.trunc()), control),
            Value::Decimal(value) => decimal(
                value
                    .trunc_to_scale_with_control(0, control)?
                    .expect("zero decimal scale"),
                control,
            ),
            Value::Null => plain(Value::Null, control),
            other => Err(SQLError::TypeMismatch(format!("trunc({other:?})"))),
        },
        2 => {
            if args.iter().any(|arg| matches!(arg, Value::Null)) {
                return plain(Value::Null, control);
            }
            if let Value::Decimal(value) = &args[0] {
                let places = to_i64_with_control(&args[1], control)?;
                let places = i32::try_from(places).map_err(|_| {
                    SQLError::TypeMismatch(format!("trunc scale out of range: {places}"))
                })?;
                return decimal(
                    value
                        .trunc_to_scale_with_control(places, control)?
                        .ok_or_else(|| SQLError::TypeMismatch("decimal trunc overflow".into()))?,
                    control,
                );
            }
            let value = to_f64_with_control(&args[0], control)?;
            let places = i32::try_from(to_i64_with_control(&args[1], control)?)
                .map_err(|_| out_of_range("integer"))?;
            let scale = 10_f64.powi(places);
            plain(Value::Float((value * scale).trunc() / scale), control)
        }
        _ => Err(SQLError::TypeMismatch("trunc takes 1 or 2 args".into())),
    }
}

fn gamma(args: &[Value], control: &ProductionControl<'_>) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch("gamma takes 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let input = to_f64_with_control(&args[0], control)?;
    if input.is_nan() || input == f64::INFINITY {
        return Ok(Value::Float(input));
    }
    if input == f64::NEG_INFINITY {
        return Err(out_of_range("double precision"));
    }
    let (result, errno) = platform_gamma::tgamma(input);
    if errno != 0 || !result.is_finite() || result == 0.0 {
        return Err(out_of_range("double precision"));
    }
    Ok(Value::Float(result))
}

fn lgamma(args: &[Value], control: &ProductionControl<'_>) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch("lgamma takes 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let input = to_f64_with_control(&args[0], control)?;
    let (result, range_error) = platform_gamma::lgamma(input);
    if range_error || (input.is_finite() && !result.is_finite()) {
        return Err(out_of_range("double precision"));
    }
    Ok(Value::Float(result))
}

fn crc32c(bytes: &[u8], control: &ProductionControl<'_>) -> Result<u32> {
    const CASTAGNOLI_REVERSED: u32 = 0x82f6_3b78;
    let mut crc = u32::MAX;
    for byte in bytes {
        control.check()?;
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (CASTAGNOLI_REVERSED & mask);
        }
    }
    Ok(!crc)
}

// PostgreSQL delegates gamma functions to the host C math library. Keeping
// this FFI boundary isolated makes native UQA builds use the same platform
// contract, while targets without a native C ABI retain the portable libm
// implementation.
#[cfg(unix)]
#[allow(unsafe_code)]
mod platform_gamma {
    #[link(name = "m")]
    unsafe extern "C" {
        #[link_name = "tgamma"]
        fn c_tgamma(value: f64) -> f64;
        #[link_name = "lgamma_r"]
        fn c_lgamma_r(value: f64, sign: *mut libc::c_int) -> f64;
    }

    pub(super) fn tgamma(value: f64) -> (f64, i32) {
        errno::set_errno(errno::Errno(0));
        // SAFETY: both C functions accept and return one binary64 value and
        // have no pointer, ownership, or lifetime preconditions.
        let result = unsafe { c_tgamma(value) };
        (result, errno::errno().0)
    }

    pub(super) fn lgamma(value: f64) -> (f64, bool) {
        errno::set_errno(errno::Errno(0));
        let mut sign = 0;
        // SAFETY: `sign` is a valid writable `c_int`; the return value is the
        // same logarithm as `lgamma` without mutating the shared `signgam`.
        let result = unsafe { c_lgamma_r(value, &raw mut sign) };
        (result, errno::errno().0 == libc::ERANGE)
    }
}

#[cfg(not(unix))]
mod platform_gamma {
    pub(super) fn tgamma(value: f64) -> (f64, i32) {
        (libm::tgamma(value), 0)
    }

    pub(super) fn lgamma(value: f64) -> (f64, bool) {
        (libm::lgamma(value), false)
    }
}

#[cfg(test)]
mod gamma_tests {
    use super::{
        gamma as gamma_controlled, lgamma as lgamma_controlled, ProductionControl, Result,
    };
    fn gamma(args: &[Value]) -> Result<Value> {
        gamma_controlled(args, &ProductionControl::uncontrolled())
    }
    fn lgamma(args: &[Value]) -> Result<Value> {
        lgamma_controlled(args, &ProductionControl::uncontrolled())
    }
    use uqa_core::Value;

    fn assert_float_close(actual: f64, expected: f64) {
        let tolerance = 8.0 * f64::EPSILON * expected.abs().max(1.0);
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual} (tolerance {tolerance})"
        );
    }

    #[test]
    fn gamma_functions_clear_stale_errno_before_native_calls() {
        #[cfg(unix)]
        errno::set_errno(errno::Errno(libc::ERANGE));
        assert_eq!(gamma(&[Value::Float(5.0)]).unwrap(), Value::Float(24.0));

        #[cfg(unix)]
        errno::set_errno(errno::Errno(libc::ERANGE));
        match lgamma(&[Value::Float(-0.5)]).unwrap() {
            Value::Float(value) => assert_float_close(value, 1.265_512_123_484_645_4),
            other => panic!("expected double precision, got {other:?}"),
        }
    }

    #[test]
    fn gamma_functions_preserve_postgresql_range_errors() {
        for result in [
            gamma(&[Value::Float(0.0)]),
            gamma(&[Value::Float(172.0)]),
            lgamma(&[Value::Float(0.0)]),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
        }
    }
}

#[cfg(test)]
mod production_tests;
