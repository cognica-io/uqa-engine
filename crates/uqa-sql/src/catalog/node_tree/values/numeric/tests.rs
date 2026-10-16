//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn numeric_datum_production_releases_scratch_and_checks_both_cancellations() {
    for text in [
        "0".to_string(),
        "-0.00000".into(),
        "-12.003400".into(),
        "NaN".into(),
        "Infinity".into(),
        "-Infinity".into(),
        format!("{}.{}1", "9".repeat(8192), "0".repeat(129)),
    ] {
        let number = DecimalValue::parse(&text).unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let bytes = encode_with_control(&number, &control).unwrap();
        assert_eq!(bytes.reserved_bytes(), bytes.capacity());
        assert_eq!(budget.used(), bytes.capacity());
        assert_eq!(decode(&bytes).unwrap(), number.to_sql_string());
        drop(bytes);
        assert_eq!(budget.used(), 0);
        let empty = MemoryBudget::new(0);
        assert_eq!(
            encode_with_control(
                &number,
                &ProductionControl::new(&empty, &original, &invoking)
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
        assert_eq!(empty.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                encode_with_control(&number, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(budget.used(), 0);
        }
    }
}
