//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn array_literal_rejects_postgresql_unrepresentable_upper_bound() {
    let error = parse_pg_array_literal("[2147483647:2147483647]={1}").unwrap_err();
    assert_eq!(error.sqlstate(), Some("54000"));
    assert_eq!(
        error.to_string(),
        "array upper bound is too large: 2147483647"
    );
    assert!(parse_pg_array_literal("[2147483646:2147483646]={1}").is_ok());
}
use uqa_core::DecimalValue;

#[test]
fn void_casts_are_limited_to_postgresql_string_categories() {
    assert_eq!(
        cast_value_from(&Value::Str("ignored".into()), "void", Some("text")).unwrap(),
        Value::Void
    );
    assert_eq!(
        cast_value_from(&Value::Void, "character varying", Some("void")).unwrap(),
        Value::Str(String::new())
    );
    assert_eq!(
        cast_value_from(&Value::Null, "void", Some("text")).unwrap(),
        Value::Null
    );
    for (value, target, source, message) in [
        (
            Value::Int(1),
            "void",
            "integer",
            "cannot cast type integer to void",
        ),
        (
            Value::Void,
            "integer",
            "void",
            "cannot cast type void to integer",
        ),
    ] {
        let error = cast_value_from(&value, target, Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
        assert_eq!(error.to_string(), message);
    }
    let error = cast_value(&Value::Null, "void[]").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(error.to_string(), "type \"void[]\" does not exist");
}

#[test]
fn temporal_cross_casts_convert_the_carrier_kind() {
    let date = Value::Temporal(TemporalValue::parse_date("2020-01-02").unwrap());
    assert_eq!(
        cast_value(&date, "timestamp").unwrap(),
        Value::Temporal(TemporalValue::parse_timestamp("2020-01-02 00:00:00").unwrap())
    );
    let timestamp = Value::Temporal(TemporalValue::parse_timestamp("2020-01-02 03:04:05").unwrap());
    assert_eq!(
        cast_value(&timestamp, "date").unwrap(),
        Value::Temporal(TemporalValue::parse_date("2020-01-02").unwrap())
    );
    assert_eq!(
        cast_value(&timestamp, "time").unwrap(),
        Value::Temporal(TemporalValue::parse_time("03:04:05").unwrap())
    );
    let interval = Value::Temporal(TemporalValue::parse_interval("1 day 25:02:03").unwrap());
    assert_eq!(
        cast_value(&interval, "time").unwrap(),
        Value::Temporal(TemporalValue::parse_time("01:02:03").unwrap())
    );
}

#[test]
fn uuid_cast_matches_postgresql_input_and_canonical_output() {
    for input in [
        "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
        "a0eebc999c0b4ef8bb6d6bb9bd380a11",
        "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
        "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
    ] {
        assert_eq!(
            cast_value(&Value::Str(input.into()), "uuid").unwrap(),
            Value::Str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into())
        );
    }
}

#[test]
fn uuid_cast_rejects_postgresql_invalid_forms() {
    for input in [
        " a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11 ",
        "a0e-ebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
        "not-a-uuid",
    ] {
        let error = cast_value(&Value::Str(input.into()), "uuid").unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
}

#[test]
fn oid_cast_preserves_postgresql_source_type_rules() {
    assert_eq!(
        cast_value_from(&Value::Int(-1), "oid", Some("smallint")).unwrap(),
        Value::Int(i64::from(u32::MAX))
    );
    assert_eq!(
        cast_value_from(&Value::Int(-1), "oid", Some("integer")).unwrap(),
        Value::Int(i64::from(u32::MAX))
    );
    assert_eq!(
        cast_value_from(&Value::Int(i64::from(u32::MAX)), "oid", Some("bigint")).unwrap(),
        Value::Int(i64::from(u32::MAX))
    );
    let error = cast_value_from(&Value::Int(-1), "oid", Some("bigint")).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22003"));
    assert_eq!(error.to_string(), "OID out of range");
    for source in ["boolean", "numeric", "double precision"] {
        let value = match source {
            "boolean" => Value::Bool(true),
            "numeric" => Value::Decimal(DecimalValue::from_i64(1)),
            _ => Value::Float(1.0),
        };
        let error = cast_value_from(&value, "oid", Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
    }
}

#[test]
fn regclass_cast_preserves_bound_relation_names_and_oid_carriers() {
    assert_eq!(
        cast_value_from(
            &Value::Str("app.items".into()),
            "pg_catalog.regclass",
            Some("unknown")
        )
        .unwrap(),
        Value::Str("app.items".into())
    );
    assert_eq!(
        cast_value_from(&Value::Int(2205), "regclass", Some("oid")).unwrap(),
        Value::Int(2205)
    );
}

#[test]
fn regtype_zero_uses_postgresql_dash_text_output() {
    for source in [
        "regproc",
        "regprocedure",
        "regclass",
        "regnamespace",
        "regtype",
    ] {
        assert_eq!(
            cast_value_from(&Value::Int(0), "text", Some(source)).unwrap(),
            Value::Str("-".into()),
            "{source}"
        );
    }
    assert_eq!(
        cast_value_from(&Value::Int(42), "text", Some("regproc")).unwrap(),
        Value::Str("42".into())
    );
}

#[test]
fn oid_and_xid_text_input_use_postgresql_uint32_syntax() {
    for target in ["oid", "xid"] {
        assert_eq!(
            cast_value(&Value::Str("-1".into()), target).unwrap(),
            Value::Int(i64::from(u32::MAX))
        );
        assert_eq!(
            cast_value(&Value::Str(i32::MIN.to_string()), target).unwrap(),
            Value::Int(i64::from(i32::MIN as u32))
        );
        assert_eq!(
            cast_value(&Value::Str(u32::MAX.to_string()), target).unwrap(),
            Value::Int(i64::from(u32::MAX))
        );
        for input in ["-2147483649", "4294967296"] {
            let error = cast_value(&Value::Str(input.into()), target).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
        }
        let error = cast_value(&Value::Str("1.0".into()), target).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
}

#[test]
fn xid_rejects_integer_and_oid_cast_sources() {
    for source in ["smallint", "integer", "bigint", "oid"] {
        let error = cast_value_from(&Value::Int(1), "xid", Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
    }
}

#[test]
fn legacy_vector_text_casts_use_postgresql_space_separation() {
    let vector = Value::List(vec![Value::Int(23), Value::Int(25)]);
    assert_eq!(
        cast_value_from(&vector, "text", Some("oidvector")).unwrap(),
        Value::Str("23 25".into())
    );
    let stored = Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(3)]).unwrap());
    assert_eq!(
        cast_value_from(&stored, "text", Some("int2vector")).unwrap(),
        Value::Str("1 3".into())
    );
    assert_eq!(
        cast_value_from(
            &Value::List(Vec::new()),
            "text",
            Some("pg_catalog.int2vector")
        )
        .unwrap(),
        Value::Str(String::new())
    );
}

#[test]
fn bytea_cast_preserves_postgresql_source_type_and_input_rules() {
    assert_eq!(
        cast_value_from(&Value::Int(-1), "bytea", Some("smallint")).unwrap(),
        Value::Bytes(vec![0xff, 0xff])
    );
    assert_eq!(
        cast_value_from(&Value::Int(-1), "bytea", Some("integer")).unwrap(),
        Value::Bytes(vec![0xff; 4])
    );
    assert_eq!(
        cast_value_from(&Value::Int(-1), "bytea", Some("bigint")).unwrap(),
        Value::Bytes(vec![0xff; 8])
    );
    assert_eq!(
        cast_value_from(&Value::Str("\\x6162".into()), "bytea", Some("text")).unwrap(),
        Value::Bytes(b"ab".to_vec())
    );
    assert_eq!(
        cast_value_from(&Value::Str("a\\\\b\\141".into()), "bytea", Some("text")).unwrap(),
        Value::Bytes(b"a\\ba".to_vec())
    );
    for (value, source) in [
        (Value::Bool(true), "boolean"),
        (Value::Decimal(DecimalValue::from_i64(1)), "numeric"),
        (Value::Float(1.0), "double precision"),
    ] {
        let error = cast_value_from(&value, "bytea", Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
    }
    for input in ["\\x1", "\\xzz", "\\9"] {
        let error = cast_value(&Value::Str(input.into()), "bytea").unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
    }
}

#[test]
fn unary_minus_preserves_integer_width_and_overflow() {
    for (source, input, expected) in [
        ("smallint", 1_i64, -1_i64),
        ("integer", 1_i64, -1_i64),
        ("bigint", 1_i64, -1_i64),
    ] {
        assert_eq!(
            negate_value(&Value::Int(input), Some(source)).unwrap(),
            Value::Int(expected)
        );
    }
    for (source, minimum) in [
        ("smallint", i64::from(i16::MIN)),
        ("integer", i64::from(i32::MIN)),
        ("bigint", i64::MIN),
    ] {
        let error = negate_value(&Value::Int(minimum), Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
    }
}

#[test]
fn unary_minus_preserves_interval_fields() {
    assert_eq!(
        negate_value(
            &Value::Temporal(TemporalValue::Interval {
                months: 2,
                days: -3,
                micros: 4,
            }),
            Some("interval"),
        )
        .unwrap(),
        Value::Temporal(TemporalValue::Interval {
            months: -2,
            days: 3,
            micros: -4,
        })
    );
}
