//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{configuration_clauses, estimate, source_clause};

// Expected source, proconfig and float4 spellings were independently read with
// pg_get_functiondef from PostgreSQL 18.4. Only disposable probe catalog rows
// were changed for malformed lists and estimates rejected by CREATE FUNCTION.
// Captured inputs and outputs: tests/parity/pg18/routine_definition_format_oracle.expected.json.
// Reference: REL_18_4 ruleutils.c (pg_get_functiondef/simple_quote_literal),
// varlena.c (SplitGUCList) and src/port/snprintf.c (fmtfloat).

#[test]
fn source_bodies_use_postgresql_dollar_prefix_collision_rules() {
    for (source, procedure, expected) in [
        ("SELECT 1", false, "AS $function$SELECT 1$function$"),
        (
            "SELECT '$function without a final dollar'",
            false,
            "AS $functionx$SELECT '$function without a final dollar'$functionx$",
        ),
        (
            "SELECT '$function$ $functionx$ $functionxx incomplete'",
            false,
            "AS $functionxxx$SELECT '$function$ $functionx$ $functionxx incomplete'$functionxxx$",
        ),
        (
            "SELECT '$procedure$'",
            false,
            "AS $function$SELECT '$procedure$'$function$",
        ),
        (
            "SELECT '$procedurex and $procedure';",
            true,
            "AS $procedurexx$SELECT '$procedurex and $procedure';$procedurexx$",
        ),
        ("", false, "AS $function$$function$"),
        (
            "SELECT 'é''x\\path';\n-- 마지막 줄\n",
            false,
            "AS $function$SELECT 'é''x\\path';\n-- 마지막 줄\n$function$",
        ),
    ] {
        assert_eq!(source_clause(source, procedure), expected, "{source:?}");
    }
}

#[test]
fn estimates_match_postgresql_float4_cost_formatting() {
    // float4send(procost) captures the precise catalog value, including signed
    // zero, tie-to-even rounding, subnormal values and special values.
    for (bits, expected) in [
        (0x0000_0000, "0"),
        (0x8000_0000, "-0"),
        (0x3f80_0000, "1"),
        (0xbf80_0000, "-1"),
        (0x3f9e_063a, "1.23457"),
        (0x3f9e_05e6, "1.23456"),
        (0x411f_fffa, "9.99999"),
        (0x411f_fffb, "10"),
        (0x4974_23e8, "999998"),
        (0x4974_23f8, "1e+06"),
        (0x4974_2400, "1e+06"),
        (0x3727_c5ac, "1e-05"),
        (0x38d1_b717, "0.0001"),
        (0x38d1_b710, "9.99999e-05"),
        (0x4640_e680, "12345.6"),
        (0x7f7f_ffff, "3.40282e+38"),
        (0xff7f_ffff, "-3.40282e+38"),
        (0x0080_0000, "1.17549e-38"),
        (0x0000_0001, "1.4013e-45"),
        (0x7f80_0000, "Infinity"),
        (0xff80_0000, "-Infinity"),
        (0x7fc0_0000, "NaN"),
    ] {
        assert_eq!(estimate(f32::from_bits(bits)), expected, "{bits:#010x}");
    }
}

fn clauses(values: &[(&str, &str)], standard_strings: bool) -> String {
    let values = values
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect::<Vec<_>>();
    configuration_clauses(&values, standard_strings).unwrap()
}

#[test]
fn saved_configuration_preserves_order_and_parameter_name_quoting() {
    assert_eq!(
        clauses(
            &[
                ("search_path", "\"$user\", public"),
                ("work_mem", "4MB"),
                ("TimeZone", "Asia/Seoul"),
            ],
            true,
        ),
        " SET search_path TO '$user', 'public'\n SET work_mem TO '4MB'\n SET \"TimeZone\" TO 'Asia/Seoul'\n",
    );
}

#[test]
fn quoted_lists_preserve_case_empty_elements_quotes_and_long_names() {
    assert_eq!(
        clauses(
            &[
                ("search_path", "MiXeD, \"\", \"a\"\"b\", \"with space\", \"comma,name\", AbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAb한글"),
            ],
            true,
        ),
        " SET search_path TO 'MiXeD', '', 'a\"b', 'with space', 'comma,name', 'AbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAbAb한글'\n",
    );
}

#[test]
fn quoted_lists_use_postgresql_scanner_whitespace() {
    assert_eq!(
        clauses(
            &[("search_path", " \t\u{b}\r\n\u{c}MiXeD \t, \"x\"\r\n"),],
            true,
        ),
        " SET search_path TO 'MiXeD', 'x'\n",
    );
}

#[test]
fn empty_quoted_lists_have_no_rendered_elements() {
    assert_eq!(
        clauses(&[("search_path", " \t"),], true,),
        " SET search_path TO \n",
    );
}

#[test]
fn unknown_settings_are_literals_even_when_they_contain_commas() {
    assert_eq!(
        clauses(
            &[
                ("uqa.option", "a,b"),
                ("uqa.Mixed", "quote' and back\\slash"),
            ],
            true,
        ),
        " SET \"uqa.option\" TO 'a,b'\n SET \"uqa.Mixed\" TO 'quote'' and back\\slash'\n",
    );
}

#[test]
fn configuration_literals_follow_standard_conforming_strings() {
    assert_eq!(
        clauses(
            &[
                ("uqa.option", "quote' and back\\slash"),
                ("search_path", "\"quote' and back\\slash\""),
            ],
            false,
        ),
        " SET \"uqa.option\" TO 'quote'' and back\\\\slash'\n SET search_path TO 'quote'' and back\\\\slash'\n",
    );
}

#[test]
fn malformed_quoted_lists_preserve_postgresql_diagnostic() {
    for value in ["a b", "a,", "a,,b", "\"unterminated", "\"x\"tail", ",a"] {
        let error =
            configuration_clauses(&[("search_path".into(), value.into())], true).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(error.to_string(), "invalid list syntax in proconfig item");
    }
}

#[test]
fn absent_configuration_has_no_output() {
    assert_eq!(configuration_clauses(&[], true).unwrap(), "");
}
