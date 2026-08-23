//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 additions that span multiple expression families.

use super::*;

// ---------------------------------------------------------------------
// PostgreSQL 18 additions
// ---------------------------------------------------------------------

#[test]
fn pg18_array_sort_and_reverse() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2])"),
        array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Null,
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], true)"),
        array(vec![
            Value::Null,
            Value::Int(3),
            Value::Int(2),
            Value::Int(1),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], false, true)"),
        array(vec![
            Value::Null,
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_reverse(ARRAY[[1,2],[3,4]])"),
        array(vec![
            Value::List(vec![Value::Int(3), Value::Int(4)]),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[ARRAY[1,NULL],ARRAY[1,2]])"),
        array(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(1), Value::Null]),
        ])
    );
}

#[test]
fn pg18_json_strip_nulls_can_strip_array_elements() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT jsonb_strip_nulls('{\"a\":null,\"b\":[1,null,{\"c\":null}]}'::jsonb) = '{\"b\":[1,null,{}]}'::jsonb"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT jsonb_strip_nulls('{\"a\":null,\"b\":[1,null,{\"c\":null}]}'::jsonb, true) = '{\"b\":[1,{}]}'::jsonb"
        ),
        Value::Bool(true)
    );
}

#[test]
fn pg18_jsonb_numbers_use_postgresql_numeric_range() {
    let eng = engine();
    for sql in [
        "SELECT '1e131072'::jsonb",
        "SELECT '1e-16384'::jsonb",
        "SELECT '[1e131072]'::jsonb",
        "SELECT '{\"n\":1e131072}'::jsonb",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}");
    }
    assert_eq!(
        scalar(&eng, "SELECT '1e131071'::jsonb > '0'::jsonb"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT '0e200000'::jsonb = '0'::jsonb"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT json_typeof('1e200000'::json)"),
        Value::Str("number".into())
    );
}

#[test]
fn pg18_casefold_uses_full_unicode_mapping() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT casefold('Straße')"),
        Value::Str("strasse".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT casefold('Σςσ')"),
        Value::Str("σσσ".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT casefold('İIıi')"),
        Value::Str("i\u{307}iıi".into())
    );
}

#[test]
fn pg18_checksums_and_gamma_functions() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT crc32('123456789'::bytea)"),
        Value::Int(3_421_780_262)
    );
    assert_eq!(
        scalar(&eng, "SELECT crc32c('123456789'::bytea)"),
        Value::Int(3_808_858_755)
    );
    for (sql, expected) in [
        ("SELECT gamma(5)", 24.0),
        ("SELECT gamma(0.5)", 1.772_453_850_905_516),
        ("SELECT lgamma(5)", 3.178_053_830_347_945_8),
        ("SELECT lgamma(-0.5)", 1.265_512_123_484_645_4),
    ] {
        let Value::Float(actual) = scalar(&eng, sql) else {
            panic!("expected float from {sql}");
        };
        assert!((actual - expected).abs() < 1e-14, "{sql}: {actual}");
    }
    assert_eq!(
        scalar(&eng, "SELECT gamma('Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert_eq!(
        scalar(&eng, "SELECT lgamma('-Infinity'::float8)"),
        Value::Float(f64::INFINITY)
    );
    assert!(matches!(
        scalar(&eng, "SELECT gamma('NaN'::float8)"),
        Value::Float(value) if value.is_nan()
    ));
    for sql in [
        "SELECT gamma('-Infinity'::float8)",
        "SELECT gamma(0::float8)",
        "SELECT gamma(-200.5::float8)",
        "SELECT lgamma(0::float8)",
    ] {
        assert!(scalar_err(&eng, sql).contains("out of range"), "{sql}");
    }
}

#[test]
fn pg18_interval_extract_week_and_negative_quarter() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT extract(week FROM interval '20 days')"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT extract(week FROM interval '-20 days')"),
        Value::Int(-2)
    );
    for months in [-14, -12, -1] {
        assert_eq!(
            scalar(
                &eng,
                &format!("SELECT extract(quarter FROM interval '{months} months')")
            ),
            Value::Int(-1)
        );
    }
}

#[test]
fn pg18_to_number_parses_the_postgresql_roman_prefix() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT to_number(' MCMLXXXIV ', 'RN')"),
        dec("1984")
    );
    assert_eq!(
        scalar(&eng, "SELECT to_number('mcmlxxxiv', 'rn')"),
        dec("1984")
    );
    assert_eq!(scalar(&eng, "SELECT to_number('XIVjunk', 'RN')"), dec("14"));
    assert_eq!(
        scalar(&eng, "SELECT to_number('MMMDCCCLXXXVIIII', 'RN')"),
        dec("3888")
    );
    for input in ["IIII", "MCMCM", "IL", "ABC"] {
        let error = eng
            .sql(&format!("SELECT to_number('{input}', 'RN')"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"), "{input}: {error}");
        assert!(error.to_string().contains("invalid Roman numeral"));
    }
}

#[test]
fn pg18_uuid_generators_set_rfc_bits_and_monotonic_submillisecond_time() {
    let eng = engine();
    for (sql, version) in [("SELECT uuidv4()", '4'), ("SELECT uuidv7()", '7')] {
        let Value::Str(uuid) = scalar(&eng, sql) else {
            panic!("expected UUID text from {sql}");
        };
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.as_bytes()[14], version as u8);
        assert!(matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }
    let mut generated = Vec::new();
    for _ in 0..128 {
        let Value::Str(uuid) = scalar(&eng, "SELECT uuidv7()") else {
            panic!("expected UUIDv7 text");
        };
        generated.push(uuid);
    }
    assert!(
        generated.windows(2).all(|pair| pair[0] < pair[1]),
        "UUIDv7 values must be strictly ascending within a backend"
    );

    let Value::Str(unshifted) = scalar(&eng, "SELECT uuidv7()") else {
        panic!("expected unshifted UUIDv7");
    };
    let Value::Str(shifted) = scalar(&eng, "SELECT uuidv7(interval '1 day')") else {
        panic!("expected shifted UUIDv7");
    };
    assert_eq!(shifted.as_bytes()[14], b'7');
    assert!(unshifted < shifted);
    assert!(scalar_err(&eng, "SELECT uuidv7(interval '-100 years')").contains("out of range"));
}

#[test]
fn pg18_uuid_extraction_matches_rfc_variants_versions_and_timestamps() {
    let eng = engine();
    assert_uuid_versions(&eng);
    assert_uuid_timestamps(&eng);
    assert_uuid_extraction_types_and_nulls(&eng);
    assert_uuid_extraction_errors(&eng);
}

fn assert_uuid_versions(eng: &Engine) {
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_version('00000000-0000-0000-0000-000000000000')"
        ),
        Value::Null
    );
    for version in 1..=15 {
        assert_eq!(
            scalar(
                eng,
                &format!(
                    "SELECT uuid_extract_version('00000000-0000-{version:x}000-8000-000000000000')"
                )
            ),
            Value::Int(version)
        );
    }
    for variant in ["0000", "c000", "e000"] {
        assert_eq!(
            scalar(
                eng,
                &format!(
                    "SELECT uuid_extract_version('00000000-0000-7000-{variant}-000000000000')"
                )
            ),
            Value::Null
        );
    }
}

fn assert_uuid_timestamps(eng: &Engine) {
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('a8098c1a-f86e-11da-bd1a-00112444be1e')"
        ),
        Value::Temporal(TemporalValue::TimestampTz {
            micros: 1_149_936_511_013_993,
        })
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('13813fff-1dd2-11b2-8000-000000000000')"
        ),
        Value::Temporal(TemporalValue::TimestampTz { micros: -1 })
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('13814009-1dd2-11b2-8000-000000000000')"
        ),
        Value::Temporal(TemporalValue::TimestampTz { micros: 0 })
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('00000000-0001-7fff-8000-000000000000')"
        ),
        Value::Temporal(TemporalValue::TimestampTz { micros: 1_000 })
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('ffffffff-ffff-7fff-8000-000000000000')"
        ),
        Value::Temporal(TemporalValue::TimestampTz {
            micros: 281_474_976_710_655_000,
        })
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp('00000000-0000-4000-8000-000000000000')"
        ),
        Value::Null
    );
}

fn assert_uuid_extraction_types_and_nulls(eng: &Engine) {
    assert_eq!(
        scalar(
            eng,
            "SELECT pg_catalog.uuid_extract_version('00000000-0000-7000-8000-000000000000')"
        ),
        Value::Int(7)
    );
    assert_eq!(
        text(
            eng,
            "SELECT pg_typeof(uuid_extract_version('00000000-0000-7000-8000-000000000000'))"
        ),
        "smallint"
    );
    assert_eq!(
        text(
            eng,
            "SELECT pg_typeof(uuid_extract_timestamp('00000000-0000-7000-8000-000000000000'))"
        ),
        "timestamp with time zone"
    );
    assert_eq!(
        scalar(eng, "SELECT uuid_extract_version(NULL::uuid)"),
        Value::Null
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_version((SELECT '00000000-0000-7000-8000-000000000000'::uuid))"
        ),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT 1 WHERE uuid_extract_version((SELECT '00000000-0000-7000-8000-000000000000'::uuid)) = 7"
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT uuid_extract_timestamp((SELECT '00000000-0001-7000-8000-000000000000'::uuid))"
        ),
        Value::Temporal(TemporalValue::TimestampTz { micros: 1_000 })
    );
}

fn assert_uuid_extraction_errors(eng: &Engine) {
    for sql in [
        "SELECT uuid_extract_version()",
        "SELECT uuid_extract_version(1)",
        "SELECT uuid_extract_version('00000000-0000-7000-8000-000000000000'::text)",
        "SELECT uuid_extract_timestamp()",
        "SELECT uuid_extract_timestamp(1)",
        "SELECT uuid_extract_timestamp('00000000-0000-7000-8000-000000000000'::text)",
        "SELECT uuid_extract_version((SELECT '00000000-0000-7000-8000-000000000000'))",
        "SELECT uuid_extract_timestamp((SELECT '00000000-0001-7000-8000-000000000000'))",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
    let error = eng.sql("SELECT uuid_extract_version(1)", &[]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "function uuid_extract_version(integer) does not exist"
    );
    for function in ["uuid_extract_version", "uuid_extract_timestamp"] {
        let sql = format!("SELECT {function}('not-a-uuid')");
        let error = eng.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"), "{sql}: {error}");
    }
}

#[test]
fn pg18_min_and_max_accept_arrays() {
    let eng = engine();
    assert_eq!(
        scalar(
            &eng,
            "SELECT min(v) FROM (VALUES (ARRAY[2,1]),(ARRAY[1,9]),(ARRAY[2,0])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Int(9)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT max(v) FROM (VALUES (ARRAY[2,1]),(ARRAY[1,9]),(ARRAY[2,0])) AS q(v)"
        ),
        array(vec![Value::Int(2), Value::Int(1)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT min(v) FROM (VALUES (ARRAY[1,NULL]),(ARRAY[1,2])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT max(v) FROM (VALUES (ARRAY[1,NULL]),(ARRAY[1,2])) AS q(v)"
        ),
        array(vec![Value::Int(1), Value::Null])
    );
}

#[test]
fn pg18_regex_functions_accept_named_arguments() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT regexp_like(E'\\n', '[^a]', 'n')"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_like(E'\\n', '[^\\n]', 'en')"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_count(pattern => '[a-z]+', string => '123abc456def')"
        ),
        Value::Int(2)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace(replacement => 'X', string => 'abc123def456', pattern => '[0-9]+')"
        ),
        Value::Str("abcXdef456".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace(flags => 'g', replacement => 'X', string => 'abc123def456', pattern => '[0-9]+')"
        ),
        Value::Str("abcXdefX".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_substr(string => 'abc123', pattern => '([0-9]+)', start => 1, \"N\" => 1, flags => '', subexpr => 1)"
        ),
        Value::Str("123".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_instr('αβ12γ34','[0-9]+',1,2,0)"),
        Value::Int(6)
    );
    assert_eq!(
        scalar(&eng, "SELECT regexp_instr('αβ12γ34','[0-9]+',1,2,1)"),
        Value::Int(8)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT regexp_replace('abc123','([a-z]+)([0-9]+)',E'\\\\2-\\\\1-\\\\&')"
        ),
        Value::Str("123-abc-abc123".into())
    );
}
