use super::temporal::{MICROS_PER_DAY, MICROS_PER_SECOND};
use super::*;

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

/// Differential check against the previous implementation for shapes
/// unaffected by the deliberate ordinary-array/explicit-bytes change.
#[test]
fn value_json_decoding_matches_untagged_derive() {
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
            UntaggedValue::Map(map) => Value::Map(
                map.into_iter()
                    .map(|(key, value)| (key, to_value(value)))
                    .collect(),
            ),
        }
    }

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
        Value::Bytes(vec![0, 1, 255]),
        Value::Temporal(TemporalValue::Interval {
            months: 14,
            days: 3,
            micros: 4_000_000,
        }),
        Value::Decimal(DecimalValue::parse("-12.75").unwrap()),
        Value::List(vec![Value::Str("a".into()), Value::Int(300)]),
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
