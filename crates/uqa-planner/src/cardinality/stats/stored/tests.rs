//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryError;

fn row() -> StoredColumnStats<'static> {
    StoredColumnStats {
        column_name: "amount",
        distinct_count: 4,
        null_count: 1,
        min_value: Some("1"),
        max_value: Some("9"),
        row_count: 10,
        histogram_json: "[1,3,9]",
        mcv_values_json: "[3,9]",
        mcv_frequencies_json: "[0.3,0.1]",
    }
}

fn equal(actual: &ColumnStats, expected: &ColumnStats) {
    assert_eq!(actual.distinct_count, expected.distinct_count);
    assert_eq!(actual.null_count, expected.null_count);
    assert_eq!(actual.row_count, expected.row_count);
    // Serialized observations preserve tagged numeric representations, including NaN bits.
    for (actual, expected) in [
        (&actual.min_value, &expected.min_value),
        (&actual.max_value, &expected.max_value),
    ] {
        assert_eq!(
            serde_json::to_string(actual).unwrap(),
            serde_json::to_string(expected).unwrap()
        );
    }
    for (actual, expected) in [
        (&actual.histogram, &expected.histogram),
        (&actual.mcv_values, &expected.mcv_values),
    ] {
        assert_eq!(
            serde_json::to_string(actual).unwrap(),
            serde_json::to_string(expected).unwrap()
        );
    }
    assert_eq!(
        actual
            .mcv_frequencies
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        expected
            .mcv_frequencies
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
}

mod retention;

#[test]
fn stored_distributions_preserve_values_order_and_frequency_bits() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for value in [
        "null",
        "-0.0",
        "18446744073709551616",
        "1e400",
        r#""text\n\uD83D\uDE03\u0000""#,
        r#"{"$uqa_type":"float_bits","hex":"fff8000000000001"}"#,
        r#"{"$uqa_type":"bytes","hex":"00ff80"}"#,
        r#"{"$uqa_type":"record","fields":[["a",1],["a",2],["b",[1,2]]]}"#,
        r#"{"$uqa_type":"array","values":[[1,2],[3,4]],"lower_bounds":[-1,4]}"#,
        r#"{"$uqa_type":"decimal","value":"-0.000"}"#,
        r#"{"plain":[1,{"nested":true}]}"#,
    ] {
        let list = format!("[{value},2]");
        let input = StoredColumnStats {
            min_value: Some(value),
            max_value: Some(value),
            histogram_json: &list,
            mcv_values_json: &list,
            mcv_frequencies_json: "[-0.0,0.100000000000000005]",
            ..row()
        };
        let memory = MemoryBudget::new(1024 * 1024);
        let ordinary = input.decode().unwrap();
        let admitted = input
            .decode_with_control(&memory, &original, &invoking)
            .unwrap();
        equal(&admitted, &ordinary);
        assert_eq!(admitted.mcv_frequencies[0].to_bits(), (-0.0_f64).to_bits());
        assert_eq!(admitted.mcv_frequencies[1].to_bits(), 0.1_f64.to_bits());
        drop(admitted);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_distributions_keep_original_validation_order_and_diagnostics() {
    let malformed = StoredColumnStats {
        histogram_json: "!",
        mcv_values_json: "!",
        mcv_frequencies_json: "!",
        distinct_count: -1,
        null_count: -1,
        min_value: Some("!"),
        max_value: Some("!"),
        row_count: -1,
        ..row()
    };
    let cases = [
        (malformed, "expected value at line 1 column 1"),
        (
            StoredColumnStats {
                histogram_json: "[]",
                ..malformed
            },
            "expected value at line 1 column 1",
        ),
        (
            StoredColumnStats {
                histogram_json: "[]",
                mcv_values_json: "[]",
                ..malformed
            },
            "expected value at line 1 column 1",
        ),
        (
            StoredColumnStats {
                mcv_frequencies_json: "[]",
                distinct_count: -1,
                ..row()
            },
            "mismatched MCV values and frequencies for column `amount`",
        ),
        (
            StoredColumnStats {
                distinct_count: -1,
                null_count: -1,
                min_value: Some("!"),
                ..row()
            },
            "negative distinct_count for column `amount`",
        ),
        (
            StoredColumnStats {
                null_count: -1,
                min_value: Some("!"),
                ..row()
            },
            "negative null_count for column `amount`",
        ),
        (
            StoredColumnStats {
                min_value: Some("!"),
                row_count: -1,
                ..row()
            },
            "expected value at line 1 column 1",
        ),
        (
            StoredColumnStats {
                max_value: Some("!"),
                row_count: -1,
                ..row()
            },
            "expected value at line 1 column 1",
        ),
        (
            StoredColumnStats {
                row_count: -1,
                ..row()
            },
            "negative row_count for column `amount`",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(input.decode().unwrap_err().to_string(), expected);
        let memory = MemoryBudget::new(1024 * 1024);
        let token = CancellationToken::new();
        let error = input
            .decode_with_control(&memory, &token, &token)
            .unwrap_err();
        if !expected.starts_with("expected value") {
            assert_eq!(error.to_string(), expected);
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn oversized_stored_distributions_skip_bytes_but_validate_each_selected_value() {
    let too_large = "!".repeat(limits::encoded_list_bytes(limits::HISTOGRAM_VALUES) + 1);
    let many = format!("[{}]", vec!["1"; 102].join(","));
    let token = CancellationToken::new();
    for list in [&too_large, &many] {
        let input = StoredColumnStats {
            histogram_json: list,
            mcv_values_json: list,
            mcv_frequencies_json: "[]",
            min_value: Some(&too_large),
            max_value: Some("null"),
            ..row()
        };
        let ordinary = input.decode().unwrap();
        assert!(ordinary.histogram.is_empty());
        assert!(ordinary.mcv_values.is_empty());
        assert!(ordinary.min_value.is_none());
        assert!(ordinary.max_value.is_none());
        let memory = MemoryBudget::new(1024 * 1024);
        let admitted = input.decode_with_control(&memory, &token, &token).unwrap();
        equal(&admitted, &ordinary);
        assert_eq!(memory.used(), 0);
    }
    let invalid_later = format!("[{},!]", vec!["1"; 102].join(","));
    for input in [
        StoredColumnStats {
            histogram_json: &invalid_later,
            ..row()
        },
        StoredColumnStats {
            mcv_values_json: &many,
            mcv_frequencies_json: "!",
            ..row()
        },
    ] {
        assert!(input.decode().is_err());
        let memory = MemoryBudget::new(1024 * 1024);
        assert!(input.decode_with_control(&memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn omitted_wide_values_retire_their_allowance_and_keep_paired_frequencies() {
    let wide = serde_json::to_string(&"x".repeat(limits::TEXT_BYTES + 1)).unwrap();
    let list = format!("[{wide},7,{wide},9]");
    let input = StoredColumnStats {
        histogram_json: &list,
        mcv_values_json: &list,
        mcv_frequencies_json: "[0.4,0.3,0.2,0.1]",
        min_value: Some(&wide),
        ..row()
    };
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let admitted = input.decode_with_control(&memory, &token, &token).unwrap();
    assert_eq!(admitted.histogram, [Value::Int(7), Value::Int(9)]);
    assert_eq!(admitted.mcv_values, [Value::Int(7), Value::Int(9)]);
    assert_eq!(admitted.mcv_frequencies, [0.3, 0.1]);
    assert!(admitted.min_value.is_none());
    let retained = (admitted.histogram.capacity() + admitted.mcv_values.capacity())
        * size_of::<Value>()
        + admitted.mcv_frequencies.capacity() * size_of::<f64>();
    assert_eq!(admitted.reserved_bytes(), retained);
    assert_eq!(memory.used(), retained);
    drop(admitted);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_distribution_admission_retires_every_failed_prefix() {
    let input = StoredColumnStats {
        histogram_json: r#"["first","second"]"#,
        mcv_values_json: r#"["common","other"]"#,
        min_value: Some(r#""minimum""#),
        max_value: Some(r#""maximum""#),
        ..row()
    };
    let token = CancellationToken::new();
    let measured = MemoryBudget::new(1024 * 1024);
    let value = input
        .decode_with_control(&measured, &token, &token)
        .unwrap();
    let peak = measured.peak();
    assert!(measured.used() > 0);
    drop(value);
    assert_eq!(measured.used(), 0);
    for limit in 0..=peak {
        let memory = MemoryBudget::new(limit);
        let value = input.decode_with_control(&memory, &token, &token);
        if limit < peak {
            assert!(
                matches!(
                    value,
                    Err(
                        StoredColumnStatsError::Resource(ValueRetentionError::Memory(
                            MemoryError::Limit { .. }
                        )) | StoredColumnStatsError::Input(JsonReadError::Memory(
                            MemoryError::Limit { .. }
                        ))
                    )
                ),
                "limit {limit}"
            );
        } else {
            assert!(value.is_ok());
        }
        drop(value);
        assert_eq!(memory.used(), 0, "limit {limit}");
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_statistics_publication_retains_prefixes_and_both_cancellation_owners() {
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = StoredColumnStatistics::new(&memory, &original, &invoking);
        input.insert(row()).unwrap();
        let retained = memory.used();
        assert!(retained > 0);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(input.insert(row()).is_err());
        assert_eq!(memory.used(), 0);
        assert!(input.finish().is_err());
        assert_eq!(memory.used(), 0);
        assert!(row()
            .decode_with_control(&memory, &original, &invoking)
            .is_err());
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let mut input = StoredColumnStatistics::new(&memory, &token, &token);
    input.insert(row()).unwrap();
    input
        .insert(StoredColumnStats {
            row_count: 23,
            ..row()
        })
        .unwrap();
    let published = input.finish().unwrap();
    assert_eq!(published.len(), 1);
    assert_eq!(published.get("amount").unwrap().row_count, 23);
    let retained = memory.used();
    let captured = published.clone();
    drop(published);
    token.cancel();
    assert_eq!(memory.used(), retained);
    assert_eq!(captured.get("amount").unwrap().row_count, 23);
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn empty_stored_statistics_need_no_allocation() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let input = StoredColumnStats {
        histogram_json: "[]",
        mcv_values_json: "[]",
        mcv_frequencies_json: "[]",
        min_value: None,
        max_value: None,
        ..row()
    };
    let stats = input.decode_with_control(&memory, &token, &token).unwrap();
    assert_eq!(stats.reserved_bytes(), 0);
    let catalog = StoredColumnStatistics::new(&memory, &token, &token)
        .finish()
        .unwrap();
    assert!(catalog.is_empty());
    assert_eq!(memory.peak(), 0);
}

#[test]
fn stored_array_input_rejects_trailing_and_nested_malformed_values() {
    let token = CancellationToken::new();
    for text in [
        "[1] null",
        "[1,]",
        "[1",
        "{\"x\":[]}",
        "null",
        "[{}",
        "[[1],{}] ]",
        "[1, {\"x\":]}",
    ] {
        let memory = MemoryBudget::new(1024 * 1024);
        let input = StoredColumnStats {
            histogram_json: text,
            ..row()
        };
        assert!(input.decode().is_err(), "{text}");
        assert!(
            input.decode_with_control(&memory, &token, &token).is_err(),
            "{text}"
        );
        assert_eq!(memory.used(), 0, "{text}");
    }
}

#[test]
fn stored_distribution_depth_matches_the_original_container_boundary() {
    let token = CancellationToken::new();
    for depth in [1, 16, 126, 127, 128, 129] {
        let value = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let list = format!("[{value}]");
        for input in [
            StoredColumnStats {
                histogram_json: &list,
                ..row()
            },
            StoredColumnStats {
                min_value: Some(&value),
                ..row()
            },
        ] {
            let memory = MemoryBudget::new(1024 * 1024);
            let ordinary = input.decode();
            let admitted = input.decode_with_control(&memory, &token, &token);
            assert_eq!(ordinary.is_ok(), admitted.is_ok(), "depth {depth}");
            if let (Ok(ordinary), Ok(admitted)) = (ordinary, admitted) {
                equal(&admitted, &ordinary);
            }
            assert_eq!(memory.used(), 0, "depth {depth}");
        }
    }
}

#[test]
fn statistical_value_sizing_admits_formatting_and_preserves_resource_errors() {
    let token = CancellationToken::new();
    let values = [
        serde_json::from_str::<Value>("1e400").unwrap(),
        serde_json::from_str::<Value>("1e9000").unwrap(),
        Value::List(vec![Value::Bytes(vec![0x80; 512])]),
    ];
    for value in values {
        let memory = MemoryBudget::new(1024 * 1024);
        let control = ProductionControl::new(&memory, &token, &token);
        assert_eq!(
            limits::accepts_with_control(&value, &control).unwrap(),
            limits::accepts(&value)
        );
        assert!(memory.peak() > 0);
        assert_eq!(memory.used(), 0);
        let denied = MemoryBudget::new(0);
        assert!(matches!(
            limits::accepts_with_control(&value, &ProductionControl::new(&denied, &token, &token)),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(denied.used(), 0);
    }
    let input = StoredColumnStats {
        histogram_json: "[1e400]",
        mcv_values_json: "[]",
        mcv_frequencies_json: "[]",
        min_value: None,
        max_value: None,
        ..row()
    };
    let measured = MemoryBudget::new(1024 * 1024);
    drop(
        input
            .decode_with_control(&measured, &token, &token)
            .unwrap(),
    );
    for limit in 0..measured.peak() {
        let memory = MemoryBudget::new(limit);
        assert!(
            input.decode_with_control(&memory, &token, &token).is_err(),
            "limit {limit}"
        );
        assert_eq!(memory.used(), 0, "limit {limit}");
    }
}
