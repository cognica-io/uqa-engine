//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{DecimalValue, Value};

use super::format_pg_number;

#[test]
fn numeric_templates_reserve_postgresql_sign_and_digit_slots() {
    let formatted = |value, template| format_pg_number(&value, template).unwrap();
    assert_eq!(formatted(Value::Float(1234.5), "9999.99"), " 1234.50");
    assert_eq!(formatted(Value::Float(12.3), "9999.99"), "   12.30");
    assert_eq!(formatted(Value::Float(-12.3), "9999.99"), "  -12.30");
    assert_eq!(formatted(Value::Float(12.3), "0000.00"), " 0012.30");
    assert_eq!(formatted(Value::Float(12.3), "FM9999.99"), "12.3");
    assert_eq!(formatted(Value::Float(12.0), "FM9999.99"), "12.");
    assert_eq!(formatted(Value::Float(12_345.6), "9999.99"), " ####.##");
    assert_eq!(formatted(Value::Float(12_345.6), "FM9999.99"), "####.##");
    assert_eq!(formatted(Value::Float(1.0), "FM090"), "001");
    assert_eq!(formatted(Value::Float(12.0), "fm000"), "012");
    assert_eq!(formatted(Value::Float(12.0), "9999."), "   12");
    assert_eq!(formatted(Value::Float(-12.0), "FM9999."), "-12");
    assert_eq!(formatted(Value::Float(0.0), "FM9.9"), "0.");
    assert_eq!(formatted(Value::Float(0.5), "FM9.9"), ".5");
    assert_eq!(formatted(Value::Float(-1.2), "S9"), "-1");
    assert_eq!(formatted(Value::Float(1.2), "9S"), "1+");
    assert_eq!(formatted(Value::Float(12.0), "SFM999"), "+12");
    assert_eq!(formatted(Value::Float(-12.0), "FMS999"), "-12");
    assert_eq!(formatted(Value::Float(12.0), "999FMS"), "12+");
    assert_eq!(formatted(Value::Float(12.0), "9S99"), " +12");
    assert_eq!(formatted(Value::Float(-1.25), "S.9"), ".#-");
    assert_eq!(formatted(Value::Float(-1.25), "9S.9"), "1.2-");
    assert_eq!(formatted(Value::Float(-1.2), "9SG"), "1-");
    assert_eq!(formatted(Value::Float(-1.2), "9MI"), "1-");
    assert_eq!(formatted(Value::Float(-1.2), "9PR"), "<1>");
}

#[test]
fn numeric_and_float_templates_keep_their_postgresql_rounding_rules() {
    let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
    assert_eq!(format_pg_number(&decimal("2.5"), "9").unwrap(), " 3");
    assert_eq!(format_pg_number(&Value::Float(2.5), "9").unwrap(), " 2");
    assert_eq!(format_pg_number(&decimal("-2.5"), "9").unwrap(), "-3");
    assert_eq!(format_pg_number(&decimal("1.25"), "9.9").unwrap(), " 1.3");
    assert_eq!(
        format_pg_number(&Value::Float(1.25), "9.9").unwrap(),
        " 1.2"
    );
    assert_eq!(format_pg_number(&Value::Float(0.5), "9").unwrap(), " 0");
    assert_eq!(format_pg_number(&decimal("9.99"), "9.").unwrap(), " #.");
    assert_eq!(format_pg_number(&decimal("-9.99"), "9.S").unwrap(), "#.-");
    assert_eq!(format_pg_number(&decimal("-2.5"), "9.S").unwrap(), "3");
    assert_eq!(format_pg_number(&decimal("0.5"), ".9").unwrap(), " .#");
    assert_eq!(format_pg_number(&decimal("0.5"), "FM.9").unwrap(), ".#");
    assert_eq!(format_pg_number(&decimal("-0.04"), "9.9").unwrap(), "  .0");
    assert_eq!(
        format_pg_number(&Value::Float(-0.04), "9.9").unwrap(),
        " -.0"
    );
}

#[test]
fn numeric_templates_render_special_values_like_postgresql() {
    let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
    assert_eq!(
        format_pg_number(&decimal("NaN"), "99999999.99").unwrap(),
        "      NaN"
    );
    assert_eq!(
        format_pg_number(&decimal("Infinity"), "99999999.99").unwrap(),
        " Infinity"
    );
    assert_eq!(
        format_pg_number(&decimal("-Infinity"), "99999999.99").unwrap(),
        "-Infinity"
    );
    assert_eq!(format_pg_number(&decimal("NaN"), "9.9").unwrap(), " #.#");
    assert_eq!(
        format_pg_number(&decimal("-Infinity"), "99999999.99PR").unwrap(),
        "<Infinity"
    );
    assert_eq!(format_pg_number(&decimal("NaN"), "000MI").unwrap(), "NaN ");
    assert_eq!(
        format_pg_number(&decimal("Infinity"), "000PL").unwrap(),
        " ###+"
    );
}

#[test]
fn numeric_templates_preserve_unrecognized_case_and_quoted_literals() {
    let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
    assert_eq!(format_pg_number(&decimal("12"), "fM000").unwrap(), "fM 012");
    assert_eq!(format_pg_number(&decimal("-1.2"), "9Mi").unwrap(), "-1Mi");
    assert_eq!(
        format_pg_number(&decimal("12"), r#""USD"000"#).unwrap(),
        "USD 012"
    );
    assert_eq!(
        format_pg_number(&decimal("12"), r#""USD"SFM999"#).unwrap(),
        "USD+12"
    );
}

#[test]
fn numeric_templates_support_postgresql_group_currency_scale_and_suffix_tokens() {
    let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
    assert_eq!(
        format_pg_number(&decimal("1485"), "9,999").unwrap(),
        " 1,485"
    );
    assert_eq!(
        format_pg_number(&decimal("3148.5"), "9G999D999").unwrap(),
        " 3,148.500"
    );
    assert_eq!(format_pg_number(&decimal("485"), "L999").unwrap(), "$ 485");
    assert_eq!(format_pg_number(&decimal("12"), "FM00L").unwrap(), "12$");
    assert_eq!(format_pg_number(&decimal("12"), "FM00B").unwrap(), "12");
    assert_eq!(format_pg_number(&decimal("12"), "FM00C").unwrap(), "12");
    assert_eq!(format_pg_number(&decimal("12.45"), "99V9").unwrap(), " 125");
    assert_eq!(
        format_pg_number(&decimal("482"), "999th").unwrap(),
        " 482nd"
    );
    assert_eq!(format_pg_number(&decimal("0.5"), "FM00TH").unwrap(), "01ST");
    assert_eq!(format_pg_number(&decimal("12"), "SP").unwrap(), "");
}

#[test]
fn numeric_templates_support_postgresql_roman_and_scientific_notation() {
    let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
    assert_eq!(
        format_pg_number(&decimal("485"), "RN").unwrap(),
        "        CDLXXXV"
    );
    assert_eq!(format_pg_number(&decimal("5.2"), "FMRN").unwrap(), "V");
    assert_eq!(
        format_pg_number(&decimal("0"), "RN").unwrap(),
        "###############"
    );
    assert_eq!(
        format_pg_number(&decimal("0.0004859"), "9.99EEEE").unwrap(),
        " 4.86e-04"
    );
    assert_eq!(
        format_pg_number(&decimal("0.0004859"), "9D99EEEE").unwrap(),
        " 4.86e-04"
    );
    assert_eq!(
        format_pg_number(&decimal("0.0004859"), "9d99EEEE").unwrap(),
        " 4.86e-04"
    );
    assert_eq!(
        format_pg_number(&decimal("9.99"), "9.9EEEE").unwrap(),
        " 10.0e+00"
    );
    assert_eq!(
        format_pg_number(&decimal("NaN"), "9.99EEEE").unwrap(),
        " #.######"
    );
    assert_eq!(
        format_pg_number(&decimal("-Infinity"), "9.99EEEE").unwrap(),
        " #.######"
    );
    assert_eq!(
        format_pg_number(&Value::Float(0.5), "RN").unwrap(),
        "###############"
    );
    assert_eq!(format_pg_number(&Value::Float(2.5), "FMRN").unwrap(), "II");
    assert_eq!(
        format_pg_number(&Value::Float(9.99), "9.9EEEE").unwrap(),
        " 1.0e+01"
    );
    assert_eq!(
        format_pg_number(&Value::Float(-0.0), "9.9EEEE").unwrap(),
        "-0.0e+00"
    );
}

#[test]
fn float_templates_apply_postgresql_precision_before_format_processing() {
    let formatted = |value, template| format_pg_number(&Value::Float(value), template).unwrap();
    assert_eq!(formatted(1e20, "9.9"), " #.");
    assert_eq!(formatted(-1e20, "9.9"), "-#.");
    assert_eq!(formatted(1e20, "9.9MI"), "#.");
    assert_eq!(formatted(-1e20, "9.9MI"), "#.");
    assert_eq!(formatted(1e20, "9.9PL"), " #.");
    assert_eq!(formatted(-1e20, "9.9PL"), "-#.");
    assert_eq!(formatted(1e20, "9.9S"), "#.+");
    assert_eq!(formatted(-1e20, "9.9S"), "#.-");
    assert_eq!(formatted(1e20, "9.9PR"), " #. ");
    assert_eq!(formatted(-1e20, "9.9PR"), "<#.>");
    assert_eq!(formatted(1e20, "FM9.9PL"), "#.+");
    assert_eq!(formatted(-1e20, "FM9.9MI"), "#.-");
    assert_eq!(formatted(1e20, "FM9.0PL"), "#.");
    assert_eq!(formatted(1e20, "FM9.9SG"), "#.+");
    assert_eq!(formatted(1e20, "9.9MIPL"), "#.");
    assert_eq!(formatted(1e20, "FM9.9MIPL"), "#.+");
    assert_eq!(formatted(1e20, "FM09.90PL"), "##.");
    assert_eq!(formatted(-1e20, "FM09.90MI"), "##.");
    assert_eq!(formatted(-1e20, "FM09.90SG"), "##.");
    assert_eq!(
        formatted(-12_345_678_901_234.0, "FM99999999999999.09MI"),
        "12345678901234.0-"
    );
    assert_eq!(
        formatted(-12_345_678_901_234.0, "FM99999999999999.90MI"),
        "12345678901234.0"
    );
    assert_eq!(
        formatted(12_345_678_901_234.0, "99999999999999.9999"),
        " 12345678901234.0"
    );
}
