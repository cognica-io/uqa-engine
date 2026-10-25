//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, JsonValueDecoder, TemporalInfinity, Value};

#[test]
fn predecessor_temporal_tags_preserve_instants_offsets_and_infinity() {
    for (source, expected) in [
        (
            r#"{"$uqa_type":"timestamp","micros":0}"#,
            TemporalValue::Timestamp {
                postgres_micros: -POSTGRES_EPOCH_MICROS,
            },
        ),
        (
            r#"{"$uqa_type":"timestamp_tz","micros":9223372036854775807}"#,
            TemporalValue::TimestampTz {
                postgres_micros: i64::MAX - POSTGRES_EPOCH_MICROS,
            },
        ),
        (
            r#"{"$uqa_type":"timestamp","micros":-9223372036854775808}"#,
            TemporalInfinity::Negative.timestamp(),
        ),
        (
            r#"{"$uqa_type":"timestamp_tz","micros":-9223372036854775807}"#,
            TemporalInfinity::Positive.timestamp_tz(),
        ),
        (
            r#"{"$uqa_type":"time_tz","micros":86400000000,"offset_minutes":-959}"#,
            TemporalValue::TimeTz {
                micros: 86_400_000_000,
                offset_seconds: -57_540,
            },
        ),
    ] {
        assert_eq!(
            serde_json::from_str::<TemporalValue>(source).unwrap(),
            expected
        );
        assert_eq!(
            serde_json::from_str::<Value>(source).unwrap(),
            Value::Temporal(expected.clone())
        );
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let actual = JsonValueDecoder::new(&memory, &token)
            .value(source)
            .unwrap();
        assert_eq!(*actual, Value::Temporal(expected));
    }
}

#[test]
fn current_temporal_tags_distinguish_epoch_units_and_physical_endpoints() {
    for (value, tag, field) in [
        (
            TemporalValue::Timestamp {
                postgres_micros: 9_223_371_331_199_999_999,
            },
            "postgres_timestamp",
            "postgres_micros",
        ),
        (
            TemporalValue::TimestampTz {
                postgres_micros: i64::MIN + 1,
            },
            "postgres_timestamp_tz",
            "postgres_micros",
        ),
        (
            TemporalValue::TimeTz {
                micros: 86_400_000_000,
                offset_seconds: -57_599,
            },
            "time_tz_seconds",
            "offset_seconds",
        ),
    ] {
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(encoded["$uqa_type"], tag);
        assert!(encoded.get(field).is_some());
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<TemporalValue>(&text).unwrap(), value);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            Value::Temporal(value)
        );
    }
    for source in [
        r#"{"$uqa_type":"timestamp","micros":-9223372036854775806}"#,
        r#"{"$uqa_type":"time_tz","micros":0,"offset_minutes":2147483647}"#,
        r#"{"$uqa_type":"postgres_timestamp","micros":0}"#,
        r#"{"$uqa_type":"time_tz_seconds","micros":0,"offset_minutes":0}"#,
    ] {
        assert!(
            serde_json::from_str::<TemporalValue>(source).is_err(),
            "{source}"
        );
        assert!(matches!(
            serde_json::from_str::<Value>(source).unwrap(),
            Value::Map(_)
        ));
    }
}
