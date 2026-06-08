//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Additional coverage for `test_scalar_functions`.

use uqa_core::Value;
use uqa_engine::Engine;

fn one_row_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO t (id, name) VALUES (1, 'alpha')", &[])
        .unwrap();
    engine
}

fn scalar(sql: &str) -> Value {
    let engine = one_row_engine();
    let result = engine.sql(sql, &[]).unwrap();
    result.rows[0][&result.columns[0]].clone()
}

fn as_float(value: &Value) -> f64 {
    match value {
        Value::Float(v) => *v,
        Value::Int(v) => *v as f64,
        other => panic!("expected numeric value, got {other:?}"),
    }
}

#[test]
fn greatest_least_nullif_coverage_cases() {
    assert_eq!(scalar("SELECT GREATEST(1, 5, 3) FROM t"), Value::Int(5));
    assert_eq!(scalar("SELECT GREATEST(1, NULL, 3) FROM t"), Value::Int(3));
    assert_eq!(scalar("SELECT GREATEST(NULL, NULL) FROM t"), Value::Null);
    assert_eq!(scalar("SELECT LEAST(10, 5, 8) FROM t"), Value::Int(5));
    assert_eq!(scalar("SELECT LEAST(10, NULL, 3) FROM t"), Value::Int(3));
    assert_eq!(scalar("SELECT NULLIF(NULL, NULL) FROM t"), Value::Null);
}

#[test]
fn string_functions_coverage_cases() {
    assert_eq!(
        scalar("SELECT POSITION('lo' IN 'hello world') FROM t"),
        Value::Int(4)
    );
    assert_eq!(
        scalar("SELECT POSITION('xyz' IN 'hello') FROM t"),
        Value::Int(0)
    );
    assert_eq!(scalar("SELECT CHAR_LENGTH('hello') FROM t"), Value::Int(5));
    assert_eq!(
        scalar("SELECT CHARACTER_LENGTH('hello') FROM t"),
        Value::Int(5)
    );
    assert_eq!(
        scalar("SELECT STRPOS('hello world', 'lo') FROM t"),
        Value::Int(4)
    );
    assert_eq!(
        scalar("SELECT LPAD('hi', 5, 'x') FROM t"),
        Value::Str("xxxhi".into())
    );
    assert_eq!(
        scalar("SELECT LPAD('hi', 5) FROM t"),
        Value::Str("   hi".into())
    );
    assert_eq!(
        scalar("SELECT LPAD('hello', 3) FROM t"),
        Value::Str("hel".into())
    );
    assert_eq!(
        scalar("SELECT RPAD('hi', 5, 'x') FROM t"),
        Value::Str("hixxx".into())
    );
    assert_eq!(
        scalar("SELECT REPEAT('ab', 3) FROM t"),
        Value::Str("ababab".into())
    );
    assert_eq!(
        scalar("SELECT REVERSE('hello') FROM t"),
        Value::Str("olleh".into())
    );
    assert_eq!(
        scalar("SELECT SPLIT_PART('a,b,c', ',', 2) FROM t"),
        Value::Str("b".into())
    );
    assert_eq!(
        scalar("SELECT SPLIT_PART('a,b', ',', 5) FROM t"),
        Value::Str(String::new())
    );
}

#[test]
fn math_functions_coverage_cases() {
    assert!((as_float(&scalar("SELECT POWER(2, 10) FROM t")) - 1024.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT POW(3, 2) FROM t")) - 9.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT SQRT(16) FROM t")) - 4.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT LOG(100) FROM t")) - 2.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT LOG(2, 8) FROM t")) - 3.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT LN(1) FROM t")) - 0.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT EXP(0) FROM t")) - 1.0).abs() < 1e-9);
    assert_eq!(scalar("SELECT MOD(10, 3) FROM t"), Value::Int(1));
    assert!((as_float(&scalar("SELECT TRUNC(3.7) FROM t")) - 3.0).abs() < 1e-9);
    assert!((as_float(&scalar("SELECT TRUNC(3.456, 2) FROM t")) - 3.45).abs() < 1e-9);
    assert_eq!(scalar("SELECT SIGN(42) FROM t"), Value::Int(1));
    assert_eq!(scalar("SELECT SIGN(-5) FROM t"), Value::Int(-1));
    assert_eq!(scalar("SELECT SIGN(0) FROM t"), Value::Int(0));
    assert!((as_float(&scalar("SELECT RANDOM() FROM t"))).is_finite());
}

#[test]
fn null_scalar_functions_return_null() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE vals (x DOUBLE PRECISION, s TEXT)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO vals VALUES (NULL, NULL)", &[])
        .unwrap();

    let result = engine
        .sql(
            "SELECT \
             LENGTH(s) AS len_s, \
             OCTET_LENGTH(s) AS bytes_s, \
             ROUND(x) AS round_x, \
             ROUND(x, 2) AS round2_x, \
             SQRT(x) AS sqrt_x, \
             COS(x) AS cos_x, \
             SIN(x) AS sin_x, \
             TAN(x) AS tan_x, \
             FLOOR(x) AS floor_x, \
             CEIL(x) AS ceil_x, \
             TRUNC(x) AS trunc_x, \
             SIGN(x) AS sign_x \
             FROM vals",
            &[],
        )
        .unwrap();

    for column in &result.columns {
        assert_eq!(result.rows[0][column], Value::Null, "{column}");
    }
}

#[test]
fn step7_string_functions_coverage_cases() {
    assert_eq!(
        scalar("SELECT initcap('hello world') FROM t"),
        Value::Str("Hello World".into())
    );
    assert_eq!(
        scalar("SELECT translate('12345', '143', 'ax') FROM t"),
        Value::Str("a2x5".into())
    );
    assert_eq!(scalar("SELECT ascii('A') FROM t"), Value::Int(65));
    assert_eq!(scalar("SELECT chr(65) FROM t"), Value::Str("A".into()));
    assert_eq!(
        scalar("SELECT starts_with(name, 'alp') FROM t WHERE id = 1"),
        Value::Bool(true)
    );
}

#[test]
fn octet_md5_format_regex_overlay_coverage_cases() {
    assert_eq!(scalar("SELECT octet_length('hello') FROM t"), Value::Int(5));
    assert_eq!(
        scalar("SELECT md5('hello') FROM t"),
        Value::Str("5d41402abc4b2a76b9719d911017c592".into())
    );
    assert_eq!(
        scalar("SELECT format('Hello %s, you are %s', 'World', 'great') FROM t"),
        Value::Str("Hello World, you are great".into())
    );
    assert_eq!(
        scalar("SELECT regexp_match('foobarbaz', 'b(.)r') FROM t"),
        Value::List(vec![Value::Str("a".into())])
    );
    assert_eq!(
        scalar("SELECT regexp_match('hello', 'xyz') FROM t"),
        Value::Null
    );
    assert_eq!(
        scalar("SELECT regexp_replace('hello world', 'world', 'there') FROM t"),
        Value::Str("hello there".into())
    );
    assert_eq!(
        scalar("SELECT regexp_replace('aaa', 'a', 'b', 'g') FROM t"),
        Value::Str("bbb".into())
    );
    assert_eq!(
        scalar("SELECT overlay('Txxxxas' placing 'hom' from 2 for 4) FROM t"),
        Value::Str("Thomas".into())
    );
}

#[test]
fn extended_math_coverage_cases() {
    assert!((as_float(&scalar("SELECT cbrt(27) FROM t")) - 3.0).abs() < 0.001);
    assert!((as_float(&scalar("SELECT cbrt(-8) FROM t")) + 2.0).abs() < 0.001);
    assert!(as_float(&scalar("SELECT sin(0) FROM t")).abs() < 0.001);
    assert!((as_float(&scalar("SELECT cos(0) FROM t")) - 1.0).abs() < 0.001);
    assert!(as_float(&scalar("SELECT tan(0) FROM t")).abs() < 0.001);
    assert!(
        (as_float(&scalar("SELECT asin(1) FROM t")) - std::f64::consts::FRAC_PI_2).abs() < 0.001
    );
    assert!(as_float(&scalar("SELECT acos(1) FROM t")).abs() < 0.001);
    assert!(
        (as_float(&scalar("SELECT atan(1) FROM t")) - std::f64::consts::FRAC_PI_4).abs() < 0.001
    );
    assert!(
        (as_float(&scalar("SELECT atan2(1, 1) FROM t")) - std::f64::consts::FRAC_PI_4).abs()
            < 0.001
    );
    assert!((as_float(&scalar("SELECT degrees(pi()) FROM t")) - 180.0).abs() < 0.001);
    assert!((as_float(&scalar("SELECT radians(180) FROM t")) - std::f64::consts::PI).abs() < 0.001);
    assert_eq!(scalar("SELECT div(7, 2) FROM t"), Value::Int(3));
    assert_eq!(scalar("SELECT div(-7, 2) FROM t"), Value::Int(-4));
    assert_eq!(scalar("SELECT gcd(12, 8) FROM t"), Value::Int(4));
    assert_eq!(scalar("SELECT lcm(12, 8) FROM t"), Value::Int(24));
}

#[test]
fn width_bucket_coverage_cases() {
    assert_eq!(
        scalar("SELECT width_bucket(5.0, 0, 10, 5) FROM t"),
        Value::Int(3)
    );
    assert_eq!(
        scalar("SELECT width_bucket(-1, 0, 10, 5) FROM t"),
        Value::Int(0)
    );
    assert_eq!(
        scalar("SELECT width_bucket(15, 0, 10, 5) FROM t"),
        Value::Int(6)
    );
}
