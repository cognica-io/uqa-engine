//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! String-function and binary-value parity tests.

use super::*;

// ---------------------------------------------------------------------
// String functions
// ---------------------------------------------------------------------

#[test]
fn trim_family_with_character_set() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT trim(both 'x' from 'xxpadxx')"), "pad");
    assert_eq!(text(&eng, "SELECT ltrim('xxpad', 'x')"), "pad");
    assert_eq!(text(&eng, "SELECT rtrim('padxx', 'x')"), "pad");
    assert_eq!(text(&eng, "SELECT btrim('xxpadxx', 'x')"), "pad");
    // The second argument is a character SET, not a substring.
    assert_eq!(text(&eng, "SELECT ltrim('xyxpad', 'xy')"), "pad");
    assert_eq!(text(&eng, "SELECT trim('  pad  ')"), "pad");
}

#[test]
fn left_right_negative_lengths() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT left('hello', -2)"), "hel");
    assert_eq!(text(&eng, "SELECT right('hello', -2)"), "llo");
    assert_eq!(text(&eng, "SELECT left('hello', -7)"), "");
    assert_eq!(text(&eng, "SELECT right('hello', -7)"), "");
}

#[test]
fn split_part_negative_index() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -1)"), "c");
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -2)"), "b");
    assert_eq!(text(&eng, "SELECT split_part('a,b,c', ',', -4)"), "");
    assert!(scalar_err(&eng, "SELECT split_part('a,b,c', ',', 0)")
        .contains("field position must not be zero"));
}

#[test]
fn substring_clamps_window() {
    let eng = engine();
    assert_eq!(text(&eng, "SELECT substring('hello', -1, 3)"), "h");
    assert_eq!(text(&eng, "SELECT substr('hello', 0, 3)"), "he");
    assert_eq!(text(&eng, "SELECT substring('hello', 2, 3)"), "ell");
    assert_eq!(text(&eng, "SELECT substring('hello', 2)"), "ello");
    assert!(scalar_err(&eng, "SELECT substring('hello', 2, -1)")
        .contains("negative substring length not allowed"));
}

#[test]
fn new_scalar_functions() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT factorial(5)"), Value::Int(120));
    assert_eq!(scalar(&eng, "SELECT bit_length('abc')"), Value::Int(24));
    assert_eq!(text(&eng, "SELECT to_hex(255)"), "ff");
    assert_eq!(text(&eng, "SELECT to_hex(-1)"), "ffffffff");
    assert_eq!(text(&eng, "SELECT quote_ident('select')"), "\"select\"");
    assert_eq!(text(&eng, "SELECT quote_ident('hello')"), "hello");
    assert_eq!(text(&eng, "SELECT quote_ident('Hello')"), "\"Hello\"");
    assert_eq!(
        text(&eng, "SELECT quote_literal('O''Reilly')"),
        "'O''Reilly'"
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_count('a1b2c3', '[0-9]')"),
        Value::Int(3)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_like('hello', 'ell')"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT num_nulls(1, NULL, 2)"), Value::Int(1));
    assert_eq!(
        scalar(&eng, "SELECT num_nonnulls(1, NULL, 2)"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT isfinite(date '2024-01-01')"),
        Value::Bool(true)
    );
}

#[test]
fn string_to_array_pg_semantics() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('a,b,c', ',')"),
        array(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into())
        ])
    );
    // NULL separator: one element per character.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('ab', NULL)"),
        array(vec![Value::Str("a".into()), Value::Str("b".into())])
    );
    // Empty separator: whole string; empty input: empty array.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('abc', '')"),
        array(vec![Value::Str("abc".into())])
    );
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('', ',')"),
        array(vec![])
    );
    // Third argument marks NULL elements.
    assert_eq!(
        scalar(&eng, "SELECT string_to_array('a,b,c', ',', 'b')"),
        array(vec![
            Value::Str("a".into()),
            Value::Null,
            Value::Str("c".into())
        ])
    );
}

// ---------------------------------------------------------------------
// bytea
// ---------------------------------------------------------------------

#[test]
fn decode_produces_bytes() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT decode('YWJj', 'base64')"),
        Value::Bytes(b"abc".to_vec())
    );
    assert_eq!(
        text(&eng, "SELECT encode(decode('YWJj', 'base64'), 'hex')"),
        "616263"
    );
    assert_eq!(text(&eng, "SELECT encode('abc'::bytea, 'base64')"), "YWJj");
    assert_eq!(
        scalar(&eng, "SELECT reverse(decode('00ff10', 'hex'))"),
        Value::Bytes(vec![0x10, 0xff, 0x00])
    );
}
