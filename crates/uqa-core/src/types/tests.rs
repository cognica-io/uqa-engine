//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::temporal::{MICROS_PER_DAY, MICROS_PER_SECOND};
use super::*;

#[test]
fn dynamic_value_keeps_variable_width_payloads_indirect() {
    let pointer_bytes = std::mem::size_of::<usize>();
    assert_eq!(std::mem::size_of::<ArrayValue>(), pointer_bytes);
    assert_eq!(std::mem::size_of::<DecimalValue>(), pointer_bytes);
    assert!(std::mem::size_of::<Value>() <= 4 * pointer_bytes);
}

struct HostileEmptySequence;

impl<'de> serde::de::SeqAccess<'de> for HostileEmptySequence {
    type Error = serde::de::value::Error;

    fn next_element_seed<T>(
        &mut self,
        _seed: T,
    ) -> std::result::Result<Option<T::Value>, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        Ok(None)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(usize::MAX)
    }
}

#[test]
fn value_deserializer_does_not_trust_sequence_size_hints() {
    let decoder = serde::de::value::SeqAccessDeserializer::new(HostileEmptySequence);
    let value = Value::deserialize(decoder).expect("bounded sequence preallocation");
    assert_eq!(value, Value::List(Vec::new()));
}

#[test]
fn payload_default_is_zero_score_and_empty() {
    let p = Payload::default();
    assert_eq!(p.score, 0.0);
    assert!(p.positions.is_empty());
    assert!(p.fields.is_empty());
}

#[test]
fn posting_entry_construction_round_trips() {
    let e = PostingEntry::new(42, Payload::with_score(1.5));
    assert_eq!(e.doc_id, 42);
    let diff: f64 = e.payload.score - 1.5;
    assert!(diff.abs() < f64::EPSILON);
}

#[test]
fn index_stats_doc_freq_default_zero() {
    let s = IndexStats::default();
    assert_eq!(s.doc_freq("title", "rust"), 0);
}

#[test]
fn index_stats_records_doc_freq() {
    let mut s = IndexStats::default();
    s.set_doc_freq("title", "rust", 12);
    assert_eq!(s.doc_freq("title", "rust"), 12);
    assert_eq!(s.doc_freq("title", "java"), 0);
}

#[test]
fn generalized_entry_orders_lexicographically() {
    let a = GeneralizedPostingEntry {
        doc_ids: vec![1, 2],
        payload: GeneralizedPayload::default(),
    };
    let b = GeneralizedPostingEntry {
        doc_ids: vec![1, 3],
        payload: GeneralizedPayload::default(),
    };
    assert!(a < b);
}

#[test]
fn value_ordering_within_variant() {
    assert!(Value::Int(1) < Value::Int(2));
    assert!(Value::Str("a".into()) < Value::Str("b".into()));
    assert_eq!(
        Value::FixedChar("a ".into()),
        Value::FixedChar("a   ".into())
    );
}

#[test]
fn container_ordering_uses_postgresql_nulls_high_semantics() {
    let non_null = Value::List(vec![Value::Int(1), Value::Int(2)]);
    let with_null = Value::List(vec![Value::Int(1), Value::Null]);
    assert!(non_null < with_null);

    let nested_non_null = Value::List(vec![non_null]);
    let nested_with_null = Value::List(vec![with_null]);
    assert!(nested_non_null < nested_with_null);
}

#[test]
fn array_ordering_compares_row_major_contents_before_dimensions_and_bounds() {
    let one_dimensional = Value::Array(
        ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).expect("one-dimensional array"),
    );
    let two_dimensional = Value::Array(
        ArrayValue::try_new(vec![Value::List(vec![Value::Int(1), Value::Int(2)])])
            .expect("two-dimensional array"),
    );
    let different_contents = Value::Array(
        ArrayValue::try_new(vec![Value::Int(2), Value::Int(0)]).expect("one-dimensional array"),
    );
    let nested_contents = Value::Array(
        ArrayValue::try_new(vec![Value::List(vec![Value::Int(1), Value::Int(9)])])
            .expect("two-dimensional array"),
    );
    let shifted = Value::Array(
        ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![2]).expect("shifted array"),
    );
    let one_based = Value::Array(ArrayValue::try_new(vec![Value::Int(1)]).expect("array"));

    assert!(one_dimensional < two_dimensional);
    assert!(different_contents > nested_contents);
    assert!(one_based < shifted);
}

#[test]
fn value_ordering_across_variants_is_stable() {
    assert!(Value::Null < Value::Bool(false));
    // Numeric coercion: Bool(true) == 1 > Int(0).
    assert!(Value::Bool(true) > Value::Int(0));
    // Float vs Int compares numerically (not by discriminant).
    assert!(Value::Float(10.0) < Value::Int(15));
    assert!(Value::Float(20.0) > Value::Int(15));
    assert!(Value::Decimal(DecimalValue::parse("10.5").unwrap()) > Value::Int(10));
    assert_eq!(
        Value::Decimal(DecimalValue::parse("1.0").unwrap()).cmp(&Value::Int(1)),
        std::cmp::Ordering::Equal
    );

    let equivalent = [
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        Value::Decimal(DecimalValue::parse("1.0").unwrap()),
    ];
    let text = Value::Str("numeric boundary".into());
    for left in &equivalent {
        for right in &equivalent {
            assert_eq!(left.cmp(right), std::cmp::Ordering::Equal);
            assert_eq!(left.cmp(&text), right.cmp(&text));
        }
    }
}

#[test]
fn decimal_division_uses_postgresql_display_scale() {
    let divide = |left: &str, right: &str| {
        DecimalValue::parse(left)
            .unwrap()
            .checked_div_postgres(&DecimalValue::parse(right).unwrap())
            .unwrap()
            .to_sql_string()
    };

    assert_eq!(divide("1", "2"), "0.50000000000000000000");
    assert_eq!(divide("10.00", "4"), "2.5000000000000000");
    assert_eq!(divide("37569624.64", "1478"), "25419.231826792963");
    assert_eq!(divide("75.18", "1478"), "0.05086603518267929635");
    let underflow = DecimalValue::parse("0e-16383")
        .unwrap()
        .checked_div_postgres(&DecimalValue::from_i64(4))
        .unwrap();
    assert!(underflow.is_zero());
    assert_eq!(underflow.display_scale(), Some(1_000));
}

#[test]
fn decimal_scaled_division_and_square_root_keep_guard_digits() {
    let two = DecimalValue::from_i64(2);
    let three = DecimalValue::from_i64(3);
    assert_eq!(
        two.checked_div_to_scale(&three, 20)
            .unwrap()
            .to_sql_string(),
        "0.66666666666666666667"
    );
    assert_eq!(
        two.sqrt_to_scale(16).unwrap().to_sql_string(),
        "1.4142135623730950"
    );
    assert_eq!(
        DecimalValue::parse("2.0000000000000000000000000000000000000000")
            .unwrap()
            .sqrt_to_scale(16)
            .unwrap()
            .to_sql_string(),
        "1.4142135623730950"
    );
}

#[test]
fn decimal_power_preserves_postgresql_scale_and_significant_digits() {
    let power = |base: &str, exponent: &str| {
        DecimalValue::parse(base)
            .unwrap()
            .checked_pow_postgres(&DecimalValue::parse(exponent).unwrap())
            .unwrap()
            .to_sql_string()
    };

    assert_eq!(power("2", "0.5"), "1.4142135623730950");
    assert_eq!(power("2", "0.1"), "1.0717734625362932");
    assert_eq!(power("4", "0.25"), "1.4142135623730950");
    assert_eq!(
        power("0.000001", "3"),
        "0.0000000000000000010000000000000000"
    );
    assert_eq!(power("4", "3"), "64.000000000000000");
    assert_eq!(
        power("2.0000000000000000000000000000000000000000", "0.5"),
        "1.4142135623730950488016887242096980785697"
    );
    assert_eq!(power("-2", "NaN"), "NaN");
    assert_eq!(power("-2", "Infinity"), "Infinity");
    assert_eq!(power("-2", "-Infinity"), "0");
    assert_eq!(
        power("1e-1000", "-17"),
        format!("1{}.{}", "0".repeat(17_000), "0".repeat(1_000))
    );

    let high_scale_base = format!("1.{}1", "0".repeat(16_382));
    let high_scale_result = power(&high_scale_base, "0.5");
    assert_eq!(high_scale_result, format!("1.{}", "0".repeat(1_000)));
}

#[test]
fn decimal_metadata_matches_display_and_numeric_equality() {
    for text in [
        "0",
        "0.00",
        "1",
        "1.000",
        "-0.0010",
        "12345678901234567890.12345678",
        "-79228162514264337593543950335",
    ] {
        let decimal = DecimalValue::parse(text).unwrap();
        assert_eq!(decimal.sql_string_len(), decimal.to_sql_string().len());
    }

    assert_eq!(
        DecimalValue::parse("1").unwrap().canonical_parts(),
        DecimalValue::parse("1.000").unwrap().canonical_parts()
    );
    assert_eq!(
        DecimalValue::parse("0.00").unwrap().canonical_parts(),
        ("0".into(), 0)
    );
}

#[test]
fn decimal_uniform_sampling_preserves_the_range_scale_and_exact_bounds() {
    let lower = DecimalValue::parse("-1.20").unwrap();
    let upper = DecimalValue::parse("3.456").unwrap();
    let sampled = lower
        .uniform_sample_inclusive_with(&upper, || Ok::<u64, ()>(0))
        .unwrap()
        .unwrap();
    assert_eq!(sampled.to_sql_string(), "-1.200");

    let mut draws = [u64::MAX, 4_656_u64 << 51].into_iter();
    let sampled = lower
        .uniform_sample_inclusive_with(&upper, || Ok::<u64, ()>(draws.next().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(sampled.to_sql_string(), "3.456");

    let lower = DecimalValue::parse("1.0").unwrap();
    let upper = DecimalValue::parse("1.000").unwrap();
    let mut draws = 0;
    let sampled = lower
        .uniform_sample_inclusive_with(&upper, || {
            draws += 1;
            Ok::<u64, ()>(u64::MAX)
        })
        .unwrap()
        .unwrap();
    assert_eq!(sampled.to_sql_string(), "1.000");
    assert_eq!(draws, 0, "an equal range must not consume PRNG state");
}

#[test]
fn decimal_numeric_conversions_use_internal_parts() {
    let value = DecimalValue::parse("-1234567890.125").unwrap();
    assert_eq!(value.to_i64_trunc(), Some(-1_234_567_890));
    assert_eq!(value.to_f64(), Some(-1_234_567_890.125));
    assert_eq!(
        DecimalValue::parse("79228162514264337593543950335")
            .unwrap()
            .to_i64_trunc(),
        None
    );
}

#[test]
fn value_numeric_order_is_total_and_does_not_round_large_integers() {
    let rounded_float = Value::Float(9_007_199_254_740_992.0);
    let next_integer = Value::Int(9_007_199_254_740_993);
    assert!(next_integer > rounded_float);
    assert_ne!(next_integer, rounded_float);

    let nan = Value::Float(f64::NAN);
    assert_eq!(nan, Value::Float(f64::NAN));
    assert!(nan > Value::Float(f64::INFINITY));
    assert!(nan > Value::Int(i64::MAX));
    assert_eq!(Value::Float(-0.0), Value::Float(0.0));

    let huge = Value::Float(f64::MAX);
    let decimal = Value::Decimal(DecimalValue::parse("79228162514264337593543950335").unwrap());
    assert!(huge > decimal);
    assert!(Value::Float(f64::MIN_POSITIVE) > Value::Decimal(DecimalValue::from_i64(0)));
}

#[test]
fn temporal_ordering_is_overflow_safe_and_consistent_with_equality() {
    let extreme_interval = TemporalValue::Interval {
        months: i32::MAX,
        days: i32::MAX,
        micros: i64::MAX,
    };
    let smaller_interval = TemporalValue::Interval {
        months: i32::MAX,
        days: i32::MAX,
        micros: i64::MAX - 1,
    };
    assert!(extreme_interval > smaller_interval);

    let utc = TemporalValue::TimeTz {
        micros: 0,
        offset_minutes: 0,
    };
    let same_utc = TemporalValue::TimeTz {
        micros: 60 * MICROS_PER_SECOND,
        offset_minutes: 1,
    };
    assert_eq!(utc.cmp(&same_utc), std::cmp::Ordering::Equal);
    assert_eq!(utc, same_utc);

    let deserialized_extreme = TemporalValue::TimeTz {
        micros: i64::MIN,
        offset_minutes: i32::MAX,
    };
    let _ordering = deserialized_extreme.cmp(&utc);
}

#[test]
fn interval_parser_rejects_non_finite_and_overflowing_components() {
    assert_eq!(
        TemporalValue::parse_interval("1.5 days"),
        Some(TemporalValue::Interval {
            months: 0,
            days: 1,
            micros: MICROS_PER_DAY / 2,
        })
    );
    for invalid in [
        "nan seconds",
        "inf seconds",
        "1e309 seconds",
        "9223372036854775808 microseconds",
        "2562047789 hours",
        "9223372036854 seconds 1000000 microseconds",
        "9223372036854775807:00",
        "768614336404564651-0",
    ] {
        assert_eq!(
            TemporalValue::parse_interval(invalid),
            None,
            "unexpectedly accepted {invalid:?}"
        );
    }
}

#[test]
fn decimal_value_uses_tagged_json() {
    let value = Value::Decimal(DecimalValue::parse("123.4500").unwrap());
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("\"$uqa_type\":\"decimal\""));
    assert!(json.contains("\"value\":\"123.4500\""));
    assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), value);
}

fn decode(json: &str) -> Value {
    serde_json::from_str::<Value>(json).expect("decodable JSON value")
}

/// Scalar decoding retains the numeric precedence of the original
/// untagged representation; byte values now use an explicit map tag.
#[test]
fn value_json_decoding_scalar_shapes() {
    assert_eq!(decode("null"), Value::Null);
    assert_eq!(decode("true"), Value::Bool(true));
    assert_eq!(decode("false"), Value::Bool(false));
    assert_eq!(decode("42"), Value::Int(42));
    assert_eq!(decode("-42"), Value::Int(-42));
    assert_eq!(decode(&i64::MAX.to_string()), Value::Int(i64::MAX));
    assert_eq!(decode(&i64::MIN.to_string()), Value::Int(i64::MIN));
    // u64 beyond the i64 range falls through Int to Float.
    assert_eq!(decode(&u64::MAX.to_string()), Value::Float(u64::MAX as f64));
    assert_eq!(decode("1.5"), Value::Float(1.5));
    assert_eq!(decode("1.0"), Value::Float(1.0));
    assert_eq!(decode("\"hello\""), Value::Str("hello".into()));
    // Strings resolve as Str even when they look temporal.
    assert_eq!(decode("\"2024-01-01\""), Value::Str("2024-01-01".into()));
}

#[test]
fn value_json_decoding_array_shapes() {
    assert_eq!(decode("[]"), Value::List(Vec::new()));
    assert_eq!(
        decode("[1, 2, 255]"),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(255)])
    );
    assert_eq!(decode("[0]"), Value::List(vec![Value::Int(0)]));
    assert_eq!(
        decode("[1, 256]"),
        Value::List(vec![Value::Int(1), Value::Int(256)])
    );
    assert_eq!(
        decode("[1, -1]"),
        Value::List(vec![Value::Int(1), Value::Int(-1)])
    );
    assert_eq!(
        decode("[1, 2.0]"),
        Value::List(vec![Value::Int(1), Value::Float(2.0)])
    );
    assert_eq!(
        decode("[\"a\", 1]"),
        Value::List(vec![Value::Str("a".into()), Value::Int(1)])
    );
    assert_eq!(
        decode("[[3]]"),
        Value::List(vec![Value::List(vec![Value::Int(3)])])
    );
}

#[test]
fn byte_values_use_an_explicit_tagged_json_encoding() {
    let value = Value::Bytes(vec![0, 1, 15, 16, 255]);
    let json = serde_json::to_string(&value).unwrap();
    assert_eq!(json, r#"{"$uqa_type":"bytes","hex":"00010f10ff"}"#);
    assert_eq!(decode(&json), value);
    assert_eq!(
        decode(r#"{"$uqa_type":"bytes","hex":"00FF"}"#),
        Value::Bytes(vec![0, 255])
    );
}

#[test]
fn fixed_character_values_use_an_explicit_tagged_json_encoding() {
    let value = Value::FixedChar("x   ".into());
    let json = serde_json::to_string(&value).unwrap();
    assert_eq!(json, r#"{"$uqa_type":"fixed_char","value":"x   "}"#);
    assert_eq!(decode(&json), value);
}

#[test]
fn value_json_decoding_tagged_map_shapes() {
    // Temporal variants: internally tagged, deny_unknown_fields.
    assert_eq!(
        decode("{\"$uqa_type\":\"date\",\"days\":19723}"),
        Value::Temporal(TemporalValue::Date { days: 19723 })
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"time\",\"micros\":123}"),
        Value::Temporal(TemporalValue::Time { micros: 123 })
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"time_tz\",\"micros\":5,\"offset_minutes\":-90}"),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 5,
            offset_minutes: -90
        })
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"timestamp\",\"micros\":-7}"),
        Value::Temporal(TemporalValue::Timestamp { micros: -7 })
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"timestamp_tz\",\"micros\":8}"),
        Value::Temporal(TemporalValue::TimestampTz { micros: 8 })
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2,\"micros\":3}"),
        Value::Temporal(TemporalValue::Interval {
            months: 1,
            days: 2,
            micros: 3
        })
    );
    // A temporal tag with an unknown extra field fails the
    // deny_unknown_fields temporal decode and lands on Map.
    assert_eq!(
        decode("{\"$uqa_type\":\"date\",\"days\":1,\"extra\":2}"),
        Value::Map(BTreeMap::from([
            ("$uqa_type".to_string(), Value::Str("date".into())),
            ("days".to_string(), Value::Int(1)),
            ("extra".to_string(), Value::Int(2)),
        ]))
    );
    // A temporal tag with a missing field also lands on Map.
    assert_eq!(
        decode("{\"$uqa_type\":\"date\"}"),
        Value::Map(BTreeMap::from([(
            "$uqa_type".to_string(),
            Value::Str("date".into())
        ),]))
    );
    // A temporal tag with an out-of-range field value lands on Map.
    assert_eq!(
        decode("{\"$uqa_type\":\"date\",\"days\":4294967296}"),
        Value::Map(BTreeMap::from([
            ("$uqa_type".to_string(), Value::Str("date".into())),
            ("days".to_string(), Value::Int(4_294_967_296)),
        ]))
    );
    // Decimal: tagged struct without deny_unknown_fields, so extra
    // fields are tolerated.
    assert_eq!(
        decode("{\"$uqa_type\":\"decimal\",\"value\":\"1.50\"}"),
        Value::Decimal(DecimalValue::parse("1.50").unwrap())
    );
    assert_eq!(
        decode("{\"$uqa_type\":\"decimal\",\"value\":\"1.50\",\"extra\":true}"),
        Value::Decimal(DecimalValue::parse("1.50").unwrap())
    );
    // An unparseable decimal payload falls through to Map.
    assert_eq!(
        decode("{\"$uqa_type\":\"decimal\",\"value\":\"not a number\"}"),
        Value::Map(BTreeMap::from([
            ("$uqa_type".to_string(), Value::Str("decimal".into())),
            ("value".to_string(), Value::Str("not a number".into())),
        ]))
    );
    // Unknown tags fall through to Map.
    assert_eq!(
        decode("{\"$uqa_type\":\"mystery\",\"value\":1}"),
        Value::Map(BTreeMap::from([
            ("$uqa_type".to_string(), Value::Str("mystery".into())),
            ("value".to_string(), Value::Int(1)),
        ]))
    );
    // A non-string tag falls through to Map.
    assert_eq!(
        decode("{\"$uqa_type\":7}"),
        Value::Map(BTreeMap::from([("$uqa_type".to_string(), Value::Int(7)),]))
    );
}

#[test]
fn value_json_decoding_plain_maps_and_nesting() {
    assert_eq!(decode("{}"), Value::Map(BTreeMap::new()));
    assert_eq!(
        decode("{\"a\":1,\"b\":\"x\"}"),
        Value::Map(BTreeMap::from([
            ("a".to_string(), Value::Int(1)),
            ("b".to_string(), Value::Str("x".into())),
        ]))
    );
    assert_eq!(
        decode("{\"outer\":{\"$uqa_type\":\"date\",\"days\":3}}"),
        Value::Map(BTreeMap::from([(
            "outer".to_string(),
            Value::Temporal(TemporalValue::Date { days: 3 })
        ),]))
    );
    assert_eq!(
        decode("{\"list\":[\"x\",{\"$uqa_type\":\"decimal\",\"value\":\"2\"}]}"),
        Value::Map(BTreeMap::from([(
            "list".to_string(),
            Value::List(vec![
                Value::Str("x".into()),
                Value::Decimal(DecimalValue::parse("2").unwrap()),
            ])
        ),]))
    );
}

/// The untagged derive accepted serde's sequence form for
/// internally-tagged enums and tagged structs: an array whose first
/// element names (or indexes) a temporal variant - or spells
/// "decimal" - and whose remaining elements match that variant's
/// fields deserialized as Temporal / Decimal instead of List.
/// `[1,-1]` silently became `Time { micros: -1 }`. The visitor
/// deliberately drops that quirk: arrays that are not byte arrays
/// are lists, which is also the only round-trip-stable reading.
#[test]
fn value_json_decoding_keeps_arrays_as_lists() {
    assert_eq!(
        decode("[1,-1]"),
        Value::List(vec![Value::Int(1), Value::Int(-1)])
    );
    assert_eq!(
        decode("[1,256]"),
        Value::List(vec![Value::Int(1), Value::Int(256)])
    );
    assert_eq!(
        decode("[\"time\",-1]"),
        Value::List(vec![Value::Str("time".into()), Value::Int(-1)])
    );
    assert_eq!(
        decode("[\"decimal\",\"1.5\"]"),
        Value::List(vec![Value::Str("decimal".into()), Value::Str("1.5".into())])
    );
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(untagged)]
enum UntaggedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<UntaggedValue>),
    Map(BTreeMap<String, UntaggedValue>),
}

fn to_value(untagged: UntaggedValue) -> Value {
    match untagged {
        UntaggedValue::Null => Value::Null,
        UntaggedValue::Bool(value) => Value::Bool(value),
        UntaggedValue::Int(value) => Value::Int(value),
        UntaggedValue::Float(value) => Value::Float(value),
        UntaggedValue::Str(value) => Value::Str(value),
        UntaggedValue::Bytes(value) => Value::Bytes(value),
        UntaggedValue::Temporal(value) => Value::Temporal(value),
        UntaggedValue::Decimal(value) => Value::Decimal(value),
        UntaggedValue::List(items) => Value::List(items.into_iter().map(to_value).collect()),
        UntaggedValue::Map(map) => {
            if map.len() == 1 {
                if let Some(UntaggedValue::Str(number)) = map.get("$serde_json::private::Number") {
                    if let Ok(integer) = number.parse::<i64>() {
                        return Value::Int(integer);
                    }
                    if let Ok(float) = number.parse::<f64>() {
                        if float.is_finite() {
                            return Value::Float(float);
                        }
                    }
                    if let Some(decimal) = DecimalValue::parse(number) {
                        return Value::Decimal(decimal);
                    }
                }
            }
            Value::Map(
                map.into_iter()
                    .map(|(key, value)| (key, to_value(value)))
                    .collect(),
            )
        }
    }
}

/// Differential check against the previous implementation for shapes
/// unaffected by the deliberate ordinary-array/explicit-bytes change.
#[test]
fn value_json_decoding_matches_untagged_derive() {
    let corpus = [
        "null",
        "true",
        "false",
        "0",
        "-1",
        "42",
        "9223372036854775807",
        "-9223372036854775808",
        "18446744073709551615",
        "1.5",
        "1.0",
        "-0.0",
        "1e300",
        "\"\"",
        "\"hello\"",
        "\"2024-01-01\"",
        "\"$uqa_type\"",
        "[1,2.0]",
        "[\"a\"]",
        "[{\"a\":1}]",
        "{}",
        "{\"a\":1}",
        "{\"$uqa_type\":\"date\",\"days\":19723}",
        "{\"$uqa_type\":\"date\",\"days\":1,\"extra\":2}",
        "{\"$uqa_type\":\"date\"}",
        "{\"$uqa_type\":\"date\",\"days\":4294967296}",
        "{\"$uqa_type\":\"date\",\"days\":\"x\"}",
        "{\"$uqa_type\":\"date\",\"days\":1.0}",
        "{\"$uqa_type\":\"time\",\"micros\":123}",
        "{\"$uqa_type\":\"time_tz\",\"micros\":5,\"offset_minutes\":-90}",
        "{\"$uqa_type\":\"time_tz\",\"micros\":5}",
        "{\"$uqa_type\":\"timestamp\",\"micros\":-7}",
        "{\"$uqa_type\":\"timestamp_tz\",\"micros\":8}",
        "{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2,\"micros\":3}",
        "{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2}",
        "{\"$uqa_type\":\"decimal\",\"value\":\"1.50\"}",
        "{\"$uqa_type\":\"decimal\",\"value\":\"1.50\",\"extra\":true}",
        "{\"$uqa_type\":\"decimal\",\"value\":\"not a number\"}",
        "{\"$uqa_type\":\"decimal\",\"value\":7}",
        "{\"$uqa_type\":\"decimal\"}",
        "{\"$uqa_type\":\"mystery\",\"value\":1}",
        "{\"$uqa_type\":7}",
        "{\"$uqa_type\":[\"date\"]}",
        "{\"outer\":{\"$uqa_type\":\"date\",\"days\":3}}",
        "[{\"$uqa_type\":\"decimal\",\"value\":\"2\"},\"x\"]",
    ];
    for json in corpus {
        let expected = to_value(serde_json::from_str::<UntaggedValue>(json).unwrap());
        assert_eq!(
            serde_json::from_str::<Value>(json).unwrap(),
            expected,
            "visitor and untagged derive disagree for {json}"
        );
    }
}

#[test]
fn value_json_round_trips_every_variant() {
    let values = vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(-5),
        Value::Float(2.25),
        Value::Str("text".into()),
        Value::FixedChar("x   ".into()),
        Value::Bytes(vec![0, 1, 255]),
        Value::Temporal(TemporalValue::Interval {
            months: 14,
            days: 3,
            micros: 4_000_000,
        }),
        Value::Decimal(DecimalValue::parse("-12.75").unwrap()),
        Value::Json("{\"b\":2,\"a\":1}".into()),
        Value::JsonB("{\"a\": 1, \"b\": 2}".into()),
        Value::List(vec![Value::Str("a".into()), Value::Int(300)]),
        Value::Row(vec![Value::Int(1), Value::Null]),
        Value::Record(vec![
            ("key".into(), Value::Str("a".into())),
            ("value".into(), Value::Json("1".into())),
        ]),
        Value::Map(BTreeMap::from([(
            "k".to_string(),
            Value::List(vec![Value::Float(0.5)]),
        )])),
    ];
    for value in values {
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&json).unwrap(),
            value,
            "round trip failed for {json}"
        );
    }
}

#[test]
fn jsonb_equality_and_ordering_follow_postgresql_structure() {
    let jsonb = |text: &str| Value::JsonB(text.into());
    assert_eq!(jsonb("1"), jsonb("1.0"));
    assert_eq!(jsonb("1e2"), jsonb("100.00"));
    assert_eq!(jsonb("{\"b\":2,\"a\":1}"), jsonb("{\"a\":1.0,\"b\":2}"));
    assert!(jsonb("null") < jsonb("\"text\""));
    assert!(jsonb("\"text\"") < jsonb("2"));
    assert!(jsonb("2") < jsonb("true"));
    assert!(jsonb("[]") < jsonb("null"));
    assert!(jsonb("true") < jsonb("[false]"));
    assert!(jsonb("[1,9]") < jsonb("[1,10]"));
    assert!(jsonb("{\"a\":9}") < jsonb("{\"a\":0,\"b\":0}"));
    assert!(jsonb("{\"b\":1,\"zz\":1}") < jsonb("{\"c\":1,\"aa\":1}"));
    assert_eq!(
        super::jsonb_equality_key("{\"a\":1,\"b\":[2.0]}").unwrap(),
        super::jsonb_equality_key("{\"b\":[2],\"a\":1.00}").unwrap()
    );
    assert_eq!(
        jsonb("123456789012345678901234567890.123456789012345678900"),
        jsonb("123456789012345678901234567890.1234567890123456789")
    );
    assert!(
        jsonb("123456789012345678901234567890.12345678901234567891")
            > jsonb("123456789012345678901234567890.12345678901234567890")
    );
    assert_eq!(jsonb("{\"a\":1,\"a\":2}"), jsonb("{\"a\":2}"));
}
