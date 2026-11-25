//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalValue};

#[test]
fn date_templates_match_postgres_values_and_diagnostics() {
    // Captured with PostgreSQL 18.4 to_date and ISO DateStyle.
    let control = ProductionControl::uncontrolled();
    for (input, template, expected) in [
        ("29/Feb/2024", "DD/Mon/YYYY", "2024-02-29"),
        ("2024060", "YYYYDDD", "2024-02-29"),
        ("69", "YY", "2069-01-01"),
        ("70", "YY", "1970-01-01"),
        ("4714-11-24 BC", "YYYY-MM-DD BC", "4714-11-24 BC"),
        ("5874897-12-31", "YYYY-MM-DD", "5874897-12-31"),
        ("2024*3*7", "FXYYYY/MM/DD", "2024-03-07"),
        ("2024- 02- 29", "FXYYYY-MM-DD", "2024-02-29"),
        ("2024xx02xx29", r#"YYYY"ab"MM"ab"DD"#, "2024-02-29"),
        ("xxx2024", r#""cat"YYYY"#, "2024-01-01"),
        ("", "YYYY-MM-DD", "0001-01-01 BC"),
        ("0000", "YYYY", "0001-01-01 BC"),
        (
            "2024 2 28 11:15:29.35 PM",
            "YYYY MM DD HH12:MI:SS.MS PM",
            "2024-02-28",
        ),
        ("2024 31 Monday", "YYYY DD Day", "2024-01-31"),
        ("20000229", "YYYYMMDD", "2000-02-29"),
    ] {
        assert_eq!(
            parse_date(input, template, &control)
                .unwrap()
                .to_sql_string(),
            expected,
            "{input} / {template}"
        );
    }
    for (input, template, state, message) in [
        (
            "2024-02-30",
            "YYYY-MM-DD",
            "22008",
            r#"date/time field value out of range: "2024-02-30""#,
        ),
        (
            "2023 366",
            "YYYY DDD",
            "22008",
            r#"date/time field value out of range: "2023 366""#,
        ),
        (
            "5880000-01-23",
            "YYYY-MM-DD",
            "22008",
            r#"date out of range: "5880000-01-23""#,
        ),
        (
            "2024-Mxyz-01",
            "YYYY-Month-DD",
            "22007",
            r#"invalid value "Mxyz-01" for "Month""#,
        ),
        (
            "2024 00 AM",
            "YYYY HH12 AM",
            "22007",
            r#"hour "0" is invalid for the 12-hour clock"#,
        ),
        (
            "15x06x2024",
            "DD-MM-YYYY",
            "22007",
            r#"invalid value "x0" for "MM""#,
        ),
    ] {
        let err = parse_date(input, template, &control).unwrap_err();
        assert_eq!(err.sqlstate(), Some(state));
        assert_eq!(err.to_string(), message);
    }
}

#[test]
fn calendar_templates_preserve_full_range_fields_and_controlled_output() {
    // PostgreSQL 18.4 to_char, captured from uqa-pg18-projection-reference.
    for (input, template, expected) in [
        (
            "294276-12-31 23:59:59.999999",
            "YYYY-MM-DD DDD Dy Day HH24:MI:SS.US FMMonth AD",
            "294276-12-31 366 Sun Sunday    23:59:59.999999 December AD",
        ),
        (
            "4714-11-24 00:00:00 BC",
            "YYYY-MM-DD DDD Dy Day HH24:MI:SS.US FMMonth BC",
            "4714-11-24 328 Mon Monday    00:00:00.000000 November BC",
        ),
        ("2000-01-01", "\"quoted 世界\" YYYY", "quoted 世界 2000"),
    ] {
        let TemporalValue::Timestamp { postgres_micros } =
            TemporalValue::parse_timestamp(input).unwrap()
        else {
            panic!("timestamp input");
        };
        let fields = TemporalDateTime::from_postgres_micros(postgres_micros);
        let budget = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = format(fields, template, &control).unwrap();
        assert_eq!(&*output, expected);
        assert!(output.reserved_bytes() >= expected.len());
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);

        let empty = MemoryBudget::new(0);
        assert!(format(
            fields,
            template,
            &ProductionControl::new(&empty, &token, &token)
        )
        .is_err());
        assert_eq!(empty.used(), 0);
        for original_cancelled in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(format(
                fields,
                template,
                &ProductionControl::new(&budget, &original, &invoking)
            )
            .is_err());
            assert_eq!(budget.used(), 0);
        }
    }
}
