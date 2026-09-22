//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::QueryCancelled;
use num_traits::Zero;

fn decode(input: &str) -> Option<Budgeted<DecimalValue>> {
    DecimalValue::parse_budgeted(
        input,
        &MemoryBudget::new(2 * 1024 * 1024),
        &CancellationToken::new(),
    )
    .unwrap()
}

#[test]
fn borrowed_decimal_plan_preserves_special_values_signs_scales_and_exponents() {
    for (input, output, scale) in [
        (" NaN ", "NaN", None),
        ("nAn", "NaN", None),
        ("+InFiNiTy", "Infinity", None),
        ("-iNF", "-Infinity", None),
        ("\u{2003}1.20\u{a0}", "1.20", Some(2)),
        ("-0.000", "0.000", Some(3)),
        ("0e2147483647", "0", Some(0)),
        ("-000.000e2147483647", "0", Some(0)),
        ("+00012.3400e+2", "1234.00", Some(2)),
        ("0012.3400e-2", "0.123400", Some(6)),
        (".000001E+9", "1000", Some(0)),
        ("123.", "123", Some(0)),
        ("1e+0000000000000000000000000000002", "100", Some(0)),
        ("1e-0", "1", Some(0)),
    ] {
        let value = decode(input).unwrap();
        assert_eq!(value.to_sql_string(), output, "{input}");
        assert_eq!(value.display_scale(), scale, "{input}");
        assert_eq!(value.reserved_bytes(), value.retained_bytes(), "{input}");
        assert_eq!(DecimalValue::parse(input).unwrap().to_sql_string(), output);
    }
}

#[test]
fn invalid_decimal_shapes_need_no_allocation_allowance() {
    let memory = MemoryBudget::new(0);
    for input in [
        "",
        " ",
        ".",
        "+",
        "-",
        "+NaN",
        "-NaN",
        "NaN1",
        "++1",
        "--1",
        "1 2",
        "1.2.3",
        "1e",
        "1e+",
        "1e-",
        "1ee2",
        "1e2e3",
        "1e2.0",
        "1_000",
        "\u{661}",
        "1e2147483648",
        "1e-2147483649",
        "1e-2147483648",
        "0e-2147483648",
        "1e131072",
        "0e-16384",
        "1e-16384",
    ] {
        assert!(
            DecimalValue::parse_budgeted(input, &memory, &CancellationToken::new())
                .unwrap()
                .is_none(),
            "{input}"
        );
        assert!(DecimalValue::parse(input).is_none(), "{input}");
        assert_eq!(memory.used(), 0);
        assert_eq!(memory.peak(), 0);
    }
}

#[test]
fn decimal_precision_limits_count_expansion_without_charging_leading_zeros() {
    let large = decode("1e131071").unwrap();
    let text = large.to_sql_string();
    assert_eq!(text.len(), super::super::MAX_INTEGER_DIGITS);
    assert!(text.starts_with('1'));
    assert!(text[1..].bytes().all(|byte| byte == b'0'));
    let fractional = decode("1e-16383").unwrap();
    assert_eq!(fractional.display_scale(), Some(16_383));
    assert_eq!(fractional.to_sql_string().len(), 16_385);
    let invalid = format!("1{}.0", "0".repeat(super::super::MAX_INTEGER_DIGITS));
    assert!(decode(&invalid).is_none());
    let leading = format!("{}1.20", "0".repeat(32_768));
    let memory = MemoryBudget::new(conversion_bytes(3).unwrap());
    let value = DecimalValue::parse_budgeted(&leading, &memory, &CancellationToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(value.to_sql_string(), "1.20");
    assert_eq!(memory.peak(), conversion_bytes(3).unwrap());
}

#[test]
fn decimal_workspace_is_reserved_before_construction_and_only_result_bytes_remain() {
    let required = conversion_bytes(4_096).unwrap();
    let rejected = MemoryBudget::new(required - 1);
    assert!(matches!(
        DecimalValue::parse_budgeted("1e4095", &rejected, &CancellationToken::new()),
        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(rejected.used(), 0);
    assert_eq!(rejected.peak(), 0);

    let memory = MemoryBudget::new(required);
    let value = DecimalValue::parse_budgeted("1e4095", &memory, &CancellationToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(memory.peak(), required);
    assert_eq!(memory.used(), value.retained_bytes());
    assert!(memory.used() < required);
    let saved = value.to_sql_string();
    assert!(matches!(
        DecimalValue::parse_budgeted("1e4095", &memory, &CancellationToken::new()),
        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(value.to_sql_string(), saved);
    assert_eq!(memory.used(), value.retained_bytes());
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn decimal_special_and_zero_results_keep_their_box_charge() {
    for input in ["NaN", "Infinity", "-Infinity", "-0.00", "0e2147483647"] {
        let expected = DecimalValue::parse(input).unwrap();
        let memory = MemoryBudget::new(expected.retained_bytes());
        let value = DecimalValue::parse_budgeted(input, &memory, &CancellationToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(value.to_sql_string(), expected.to_sql_string());
        assert_eq!(memory.used(), expected.retained_bytes());
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn decimal_cancellation_releases_every_completed_and_partial_workspace() {
    let input = format!("{}1.{}2e10", " ".repeat(8192), "0".repeat(8192));
    let mut checks = 0;
    let memory = MemoryBudget::new(128 * 1024);
    let value = parse_with(&input, Some(&memory), &mut || {
        checks += 1;
        Ok(())
    })
    .unwrap();
    drop(value);
    assert!(checks > 12);
    assert_eq!(memory.used(), 0);
    for cancel_at in 1..=checks {
        let mut current = 0;
        assert!(matches!(
            parse_with(&input, Some(&memory), &mut || {
                current += 1;
                if current == cancel_at {
                    Err(ValueRetentionError::Cancelled(QueryCancelled))
                } else {
                    Ok(())
                }
            }),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0, "cancellation check {cancel_at}");
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        DecimalValue::parse_budgeted("1", &MemoryBudget::new(0), &cancelled),
        Err(ValueRetentionError::Cancelled(_))
    ));
}

// This independent reference keeps the former parse/expand/finite algorithm to detect changes in acceptance, sign or display scale when the borrowed constructor is changed.
fn reference_parse(input: &str) -> Option<DecimalValue> {
    let input = input.trim();
    match input.to_ascii_lowercase().as_str() {
        "nan" => return Some(DecimalValue::nan()),
        "infinity" | "+infinity" | "inf" | "+inf" => {
            return Some(DecimalValue::positive_infinity());
        }
        "-infinity" | "-inf" => return Some(DecimalValue::negative_infinity()),
        _ => {}
    }
    let negative = input.starts_with('-');
    let unsigned = input.strip_prefix(['-', '+']).unwrap_or(input);
    let mut exponent_parts = unsigned.split(['e', 'E']);
    let significand = exponent_parts.next()?;
    let exponent = exponent_parts
        .next()
        .map_or(Some(0), |text| text.parse::<i32>().ok())?;
    if exponent_parts.next().is_some() {
        return None;
    }
    let mut decimal_parts = significand.split('.');
    let integer = decimal_parts.next()?;
    let fractional = decimal_parts.next().unwrap_or("");
    if decimal_parts.next().is_some()
        || (integer.is_empty() && fractional.is_empty())
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let scale = i64::try_from(fractional.len())
        .ok()?
        .checked_sub(i64::from(exponent))?;
    let mut coefficient = BigInt::parse_bytes(format!("{integer}{fractional}").as_bytes(), 10)?;
    if negative && !coefficient.is_zero() {
        coefficient = -coefficient;
    }
    if scale < 0 {
        if coefficient.is_zero() {
            return DecimalValue::finite(coefficient, 0);
        }
        let power = u32::try_from(scale.checked_neg()?).ok()?;
        if power as usize > super::super::MAX_INTEGER_DIGITS {
            return None;
        }
        coefficient *= super::super::pow10(power);
        DecimalValue::finite(coefficient, 0)
    } else {
        DecimalValue::finite(coefficient, u32::try_from(scale).ok()?)
    }
}

#[test]
fn decimal_borrowed_and_budgeted_parsing_match_the_previous_numeric_algorithm() {
    for sign in ["", "+", "-"] {
        for integer in ["", "0", "0000", "1", "0019", "99999999999999999999"] {
            for fractional in ["", ".", ".0", ".00120", ".99999999999999999999"] {
                for exponent in ["", "e0", "E+19", "e-19", "e+0002", "e--1", "e1.0"] {
                    let input = format!(" {sign}{integer}{fractional}{exponent} ");
                    let expected = reference_parse(&input).map(|value| value.to_sql_string());
                    assert_eq!(
                        DecimalValue::parse(&input).map(|value| value.to_sql_string()),
                        expected,
                        "ordinary: {input}"
                    );
                    assert_eq!(
                        decode(&input).map(|value| value.to_sql_string()),
                        expected,
                        "budgeted: {input}"
                    );
                }
            }
        }
    }
}
