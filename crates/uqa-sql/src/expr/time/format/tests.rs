//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalValue};

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
