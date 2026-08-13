//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mathematical, padding, formatting, encoding, and split built-ins.

use super::{
    allocation_error, base64_decode, base64_encode, coerce_i64, float1, float_to_i64_trunc,
    hex_encode, md5_hex, nonnegative_usize, out_of_range, to_decimal, to_f64, to_i64,
    value_to_string, DecimalValue, Result, SQLError, Value,
};

pub(super) fn eval_math_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
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
        "random",
        "width_bucket",
        "lpad",
        "rpad",
        "repeat",
        "translate",
        "overlay",
        "format",
        "md5",
        "encode",
        "decode",
        "split_part",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            // Trig / math
            "sin" => float1(args, "sin", f64::sin),
            "cos" => float1(args, "cos", f64::cos),
            "tan" => float1(args, "tan", f64::tan),
            "asin" => float1(args, "asin", f64::asin),
            "acos" => float1(args, "acos", f64::acos),
            "atan" => float1(args, "atan", f64::atan),
            "atan2" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("atan2 takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Float(to_f64(&args[0])?.atan2(to_f64(&args[1])?)))
            }
            "sinh" => float1(args, "sinh", f64::sinh),
            "cosh" => float1(args, "cosh", f64::cosh),
            "tanh" => float1(args, "tanh", f64::tanh),
            "exp" => float1(args, "exp", f64::exp),
            "ln" => float1(args, "ln", f64::ln),
            "log" | "log10" => match args.len() {
                1 => float1(args, "log", f64::log10),
                2 => {
                    if args.iter().any(|arg| matches!(arg, Value::Null)) {
                        return Ok(Value::Null);
                    }
                    let base = to_f64(&args[0])?;
                    let v = to_f64(&args[1])?;
                    let result = v.log(base);
                    // log(numeric, numeric) is numeric in PostgreSQL and
                    // renders with 16-17 significant digits.
                    let float_input = args.iter().any(|arg| matches!(arg, Value::Float(_)));
                    if float_input {
                        Ok(Value::Float(result))
                    } else {
                        Ok(DecimalValue::parse(&format!("{result:.16}"))
                            .map_or(Value::Float(result), Value::Decimal))
                    }
                }
                _ => Err(SQLError::TypeMismatch("log takes 1 or 2 args".into())),
            },
            "log2" => float1(args, "log2", f64::log2),
            // Route cbrt through exp(ln(x)/3): this reproduces glibc's
            // last-ulp behavior (`cbrt(27)` = 3.0000000000000004), which is
            // what PostgreSQL emits on Linux builds; platform `cbrt` on
            // macOS is correctly rounded and would diverge.
            "cbrt" => float1(args, "cbrt", |x| {
                if x == 0.0 {
                    0.0
                } else {
                    x.signum() * (x.abs().ln() / 3.0).exp()
                }
            }),
            "gamma" => gamma(args),
            "lgamma" => lgamma(args),
            "crc32" | "crc32c" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
                }
                match &args[0] {
                    Value::Bytes(bytes) => {
                        let checksum = if name == "crc32" {
                            crc32fast::hash(bytes)
                        } else {
                            crc32c(bytes)
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
                Ok(Value::Int(match to_f64(&args[0])? {
                    v if v > 0.0 => 1,
                    v if v < 0.0 => -1,
                    _ => 0,
                }))
            }
            "trunc" => match args.len() {
                1 => match &args[0] {
                    Value::Int(i) => Ok(Value::Int(*i)),
                    Value::Float(f) => Ok(Value::Float(f.trunc())),
                    Value::Decimal(d) => Ok(Value::Decimal(d.trunc())),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!("trunc({other:?})"))),
                },
                2 => {
                    if args.iter().any(|arg| matches!(arg, Value::Null)) {
                        return Ok(Value::Null);
                    }
                    if matches!(args[0], Value::Decimal(_)) {
                        let places = to_i64(&args[1])?;
                        let places = i32::try_from(places).map_err(|_| {
                            SQLError::TypeMismatch(format!("trunc scale out of range: {places}"))
                        })?;
                        return to_decimal(&args[0])?
                            .trunc_to_scale(places)
                            .map(Value::Decimal)
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal trunc overflow".into())
                            });
                    }
                    let v = to_f64(&args[0])?;
                    let p =
                        i32::try_from(to_i64(&args[1])?).map_err(|_| out_of_range("integer"))?;
                    let scale = 10f64.powi(p);
                    Ok(Value::Float((v * scale).trunc() / scale))
                }
                _ => Err(SQLError::TypeMismatch("trunc takes 1 or 2 args".into())),
            },
            "pi" => Ok(Value::Float(std::f64::consts::PI)),
            "degrees" => float1(args, "degrees", f64::to_degrees),
            "radians" => float1(args, "radians", f64::to_radians),
            "random" => {
                // Lightweight pseudo-random value derived from system time.
                use std::time::{SystemTime, UNIX_EPOCH};
                let t = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0) as f64;
                Ok(Value::Float((t.sin().abs() * 1.0e9).fract()))
            }
            "width_bucket" => {
                if args.len() != 4 {
                    return Err(SQLError::TypeMismatch("width_bucket takes 4 args".into()));
                }
                let operand = to_f64(&args[0])?;
                let low = to_f64(&args[1])?;
                let high = to_f64(&args[2])?;
                let count = to_i64(&args[3])?;
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
                let overflow_bucket = count.checked_add(1).ok_or_else(|| out_of_range("bigint"))?;
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
            // Padding / formatting
            "lpad" | "rpad" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch("[lr]pad takes 2-3 args".into()));
                }
                let s = value_to_string(&args[0]);
                let n = nonnegative_usize(to_i64(&args[1])?.max(0), "lpad/rpad length")?;
                let fill = args
                    .get(2)
                    .map(value_to_string)
                    .unwrap_or_else(|| " ".into());
                let chars: Vec<char> = s.chars().collect();
                if chars.len() >= n {
                    return Ok(Value::Str(chars[..n].iter().collect()));
                }
                let need = n - chars.len();
                let fill_chars: Vec<char> = fill.chars().collect();
                if fill_chars.is_empty() {
                    return Ok(Value::Str(s));
                }
                let padding_bytes = need
                    // A Unicode scalar value occupies at most four UTF-8 bytes. Keep
                    // this literal for the workspace's Rust 1.85 MSRV; the equivalent
                    // `char::MAX_LEN_UTF8` constant was stabilized later.
                    .checked_mul(4)
                    .ok_or_else(|| allocation_error("lpad/rpad"))?;
                let capacity = s
                    .len()
                    .checked_add(padding_bytes)
                    .ok_or_else(|| allocation_error("lpad/rpad"))?;
                let mut out = String::new();
                out.try_reserve_exact(capacity)
                    .map_err(|_| allocation_error("lpad/rpad"))?;
                if name == "lpad" {
                    for i in 0..need {
                        out.push(fill_chars[i % fill_chars.len()]);
                    }
                    out.push_str(&s);
                } else {
                    out.push_str(&s);
                    for i in 0..need {
                        out.push(fill_chars[i % fill_chars.len()]);
                    }
                }
                Ok(Value::Str(out))
            }
            "repeat" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("repeat takes 2 args".into()));
                }
                let s = value_to_string(&args[0]);
                let n = nonnegative_usize(to_i64(&args[1])?.max(0), "repeat count")?;
                if n == 0 || s.is_empty() {
                    return Ok(Value::Str(String::new()));
                }
                let capacity = s
                    .len()
                    .checked_mul(n)
                    .ok_or_else(|| allocation_error("repeat"))?;
                let mut out = String::new();
                out.try_reserve_exact(capacity)
                    .map_err(|_| allocation_error("repeat"))?;
                for _ in 0..n {
                    out.push_str(&s);
                }
                Ok(Value::Str(out))
            }
            "translate" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("translate takes 3 args".into()));
                }
                let s = value_to_string(&args[0]);
                let from: Vec<char> = value_to_string(&args[1]).chars().collect();
                let to: Vec<char> = value_to_string(&args[2]).chars().collect();
                let mapped: String = s
                    .chars()
                    .filter_map(|c| match from.iter().position(|x| *x == c) {
                        Some(i) if i < to.len() => Some(to[i]),
                        Some(_) => None,
                        None => Some(c),
                    })
                    .collect();
                Ok(Value::Str(mapped))
            }
            "overlay" => {
                // OVERLAY(string PLACING substring FROM start [FOR length])
                if args.len() < 3 || args.len() > 4 {
                    return Err(SQLError::TypeMismatch("overlay takes 3 or 4 args".into()));
                }
                let s: Vec<char> = value_to_string(&args[0]).chars().collect();
                let placing: Vec<char> = value_to_string(&args[1]).chars().collect();
                let start =
                    nonnegative_usize(to_i64(&args[2])?.max(1) - 1, "overlay start position")?;
                let len = if args.len() == 4 {
                    nonnegative_usize(to_i64(&args[3])?.max(0), "overlay length")?
                } else {
                    placing.len()
                };
                let end = start.saturating_add(len).min(s.len());
                let mut out: String = s[..start.min(s.len())].iter().collect();
                out.push_str(&placing.iter().collect::<String>());
                out.push_str(&s[end..].iter().collect::<String>());
                Ok(Value::Str(out))
            }
            "format" => {
                // FORMAT('hello %s', name) -- minimal printf-style %s/%d
                // substitution. Mirrors enough of Postgres FORMAT for the
                // common cases.
                if args.is_empty() {
                    return Err(SQLError::TypeMismatch(
                        "format needs a format string".into(),
                    ));
                }
                let fmt = value_to_string(&args[0]);
                let mut out = String::with_capacity(fmt.len());
                let mut iter = fmt.chars().peekable();
                let mut idx = 1usize;
                while let Some(c) = iter.next() {
                    if c == '%' {
                        match iter.next() {
                            Some('s') | Some('I') | Some('L') => {
                                out.push_str(&value_to_string(
                                    args.get(idx).unwrap_or(&Value::Null),
                                ));
                                idx += 1;
                            }
                            Some('d') => {
                                let n = args.get(idx).and_then(|v| coerce_i64(v)).unwrap_or(0);
                                out.push_str(&n.to_string());
                                idx += 1;
                            }
                            Some('%') => out.push('%'),
                            Some(other) => out.push(other),
                            None => out.push('%'),
                        }
                    } else {
                        out.push(c);
                    }
                }
                Ok(Value::Str(out))
            }
            "md5" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("md5 takes 1 arg".into()));
                }
                Ok(Value::Str(md5_hex(value_to_string(&args[0]).as_bytes())))
            }
            "encode" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("encode takes 2 args".into()));
                }
                let owned;
                let bytes: &[u8] = match &args[0] {
                    Value::Bytes(b) => b,
                    other => {
                        owned = value_to_string(other).into_bytes();
                        &owned
                    }
                };
                let encoding = value_to_string(&args[1]);
                match encoding.as_str() {
                    "hex" => Ok(Value::Str(hex_encode(bytes))),
                    "escape" => Ok(Value::Str(
                        String::from_utf8_lossy(bytes).escape_default().collect(),
                    )),
                    "base64" => Ok(Value::Str(base64_encode(bytes))),
                    other => Err(SQLError::TypeMismatch(format!(
                        "unknown encoding {other:?}"
                    ))),
                }
            }
            "decode" => {
                // decode() produces bytea; the result renders as
                // PostgreSQL hex output (`\x616263`) at the SQL boundary.
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("decode takes 2 args".into()));
                }
                let s = value_to_string(&args[0]);
                let encoding = value_to_string(&args[1]);
                match encoding.as_str() {
                    "hex" => {
                        let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
                        if !cleaned.len().is_multiple_of(2) {
                            return Err(SQLError::TypeMismatch(
                                "invalid hexadecimal data: odd number of digits".into(),
                            ));
                        }
                        let mut out = Vec::with_capacity(cleaned.len() / 2);
                        let bytes = cleaned.as_bytes();
                        let mut i = 0;
                        while i + 1 < bytes.len() {
                            let hi = (bytes[i] as char).to_digit(16).ok_or_else(|| {
                                SQLError::TypeMismatch("invalid hexadecimal digit".into())
                            })? as u8;
                            let lo = (bytes[i + 1] as char).to_digit(16).ok_or_else(|| {
                                SQLError::TypeMismatch("invalid hexadecimal digit".into())
                            })? as u8;
                            out.push(hi * 16 + lo);
                            i += 2;
                        }
                        Ok(Value::Bytes(out))
                    }
                    "base64" => base64_decode(&s)
                        .map(Value::Bytes)
                        .map_err(|e| SQLError::TypeMismatch(format!("base64 decode: {e}"))),
                    "escape" => Ok(Value::Bytes(s.into_bytes())),
                    other => Err(SQLError::TypeMismatch(format!(
                        "unknown encoding {other:?}"
                    ))),
                }
            }
            "split_part" => {
                // Negative positions count from the end; zero errors
                // (PostgreSQL `field position must not be zero`).
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("split_part takes 3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let sep = value_to_string(&args[1]);
                let idx = to_i64(&args[2])?;
                if idx == 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: "field position must not be zero".into(),
                    });
                }
                let parts: Vec<&str> = if sep.is_empty() {
                    vec![s.as_str()]
                } else {
                    s.split(sep.as_str()).collect()
                };
                let idx_usize = if idx >= 1 {
                    match usize::try_from(idx - 1) {
                        Ok(index) => index,
                        Err(_) => return Ok(Value::Str(String::new())),
                    }
                } else {
                    let Ok(from_end) = usize::try_from(idx.unsigned_abs()) else {
                        return Ok(Value::Str(String::new()));
                    };
                    if from_end > parts.len() {
                        return Ok(Value::Str(String::new()));
                    }
                    parts.len() - from_end
                };
                Ok(Value::Str(
                    parts.get(idx_usize).copied().unwrap_or("").to_string(),
                ))
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}

fn gamma(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch("gamma takes 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let input = to_f64(&args[0])?;
    if input.is_nan() || input == f64::INFINITY {
        return Ok(Value::Float(input));
    }
    if input == f64::NEG_INFINITY {
        return Err(out_of_range("double precision"));
    }
    let result = libm::tgamma(input);
    if !result.is_finite() || result == 0.0 {
        return Err(out_of_range("double precision"));
    }
    Ok(Value::Float(result))
}

fn lgamma(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch("lgamma takes 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let input = to_f64(&args[0])?;
    let result = libm::lgamma(input);
    if input.is_finite() && !result.is_finite() {
        return Err(out_of_range("double precision"));
    }
    Ok(Value::Float(result))
}

fn crc32c(bytes: &[u8]) -> u32 {
    const CASTAGNOLI_REVERSED: u32 = 0x82f6_3b78;
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (CASTAGNOLI_REVERSED & mask);
        }
    }
    !crc
}
