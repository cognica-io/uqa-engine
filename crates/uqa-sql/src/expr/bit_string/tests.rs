//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod integration;

fn kind(case: &serde_json::Value) -> BitStringKind {
    match case["kind"].as_str().unwrap() {
        "bit" => BitStringKind::Fixed,
        "varbit" => BitStringKind::Varying,
        other => panic!("unknown reference family {other}"),
    }
}

fn execute(case: &serde_json::Value, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    let modifier = case["modifier"].as_i64().unwrap_or(-1) as i32;
    let text = case["text"].as_str().unwrap_or("");
    let value = match case["op"].as_str().unwrap() {
        "input" => input_with_control(text, kind(case), modifier, control)?,
        "coerce" | "text_cast" => {
            let value = input_with_control(text, kind(case), -1, control)?;
            coerce_with_control(
                &value,
                kind(case),
                modifier,
                case["explicit"].as_bool().unwrap_or(true),
                control,
            )?
        }
        "modifier" => {
            let length = kind(case).validate_length(case["modifiers"][0].as_i64().unwrap())?;
            return Ok(control.copy_text(&length.to_string())?);
        }
        "from_integer" => {
            let integer = case["integer"].as_str().unwrap().parse::<i64>().unwrap();
            let integer = if case["width"] == 32 {
                i64::from(i32::try_from(integer).unwrap())
            } else {
                integer
            };
            from_integer_with_control(integer, modifier, control)?
        }
        "to_integer" => {
            let value = input_with_control(text, BitStringKind::Fixed, -1, control)?;
            let integer = if case["width"] == 32 {
                i64::from(to_integer(&value)?)
            } else {
                to_bigint(&value)?
            };
            return Ok(control.copy_text(&integer.to_string())?);
        }
        other => panic!("unknown reference operation {other}"),
    };
    output_with_control(&value, control)
}

#[test]
fn bit_string_input_lengths_casts_and_integers_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 466);
    let budget = MemoryBudget::new(16 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for case in cases {
        let result = execute(case, &control);
        let expected = &case["expected"];
        if let Some(expected) = expected.as_str() {
            let value = result.unwrap_or_else(|error| panic!("{}: {error}", case["sql"]));
            assert_eq!(value.as_str(), expected, "{}", case["sql"]);
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                expected["sqlstate"].as_str(),
                "{}",
                case["sql"]
            );
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{}",
                case["sql"]
            );
            assert_eq!(
                error.detail(),
                expected["detail"].as_str(),
                "{}",
                case["sql"]
            );
            assert_eq!(error.hint(), expected["hint"].as_str(), "{}", case["sql"]);
        }
        assert_eq!(budget.used(), 0, "{}", case["sql"]);
    }
}

#[test]
fn bit_string_input_and_length_changes_keep_packed_allowances() {
    let text = "10101010".repeat(4097);
    let cancellation = CancellationToken::new();
    let budget = MemoryBudget::new(4097);
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let value = input_with_control(&text, BitStringKind::Varying, -1, &control).unwrap();
    assert_eq!(value.as_bytes(), vec![0xaa; 4097]);
    assert_eq!(value.reserved_bytes(), 4097);
    assert_eq!(
        output_with_control(&value, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 4097);
    drop(value);
    assert_eq!(budget.used(), 0);

    let too_small = MemoryBudget::new(4096);
    let limited = ProductionControl::new(&too_small, &cancellation, &cancellation);
    assert_eq!(
        input_with_control(&text, BitStringKind::Fixed, -1, &limited)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(too_small.used(), 0);

    let original = input_with_control(
        "101010101",
        BitStringKind::Fixed,
        -1,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    for (kind, length, expected) in [
        (BitStringKind::Fixed, 7, vec![0xaa]),
        (BitStringKind::Varying, 7, vec![0xaa]),
        (BitStringKind::Fixed, 17, vec![0xaa, 0x80, 0]),
        (BitStringKind::Varying, 17, vec![0xaa, 0x80]),
    ] {
        let allowance = MemoryBudget::new(expected.len());
        let control = ProductionControl::new(&allowance, &cancellation, &cancellation);
        let converted = coerce_with_control(&original, kind, length, true, &control).unwrap();
        assert_eq!(converted.as_bytes(), expected);
        assert_eq!(converted.reserved_bytes(), expected.len());
        assert_eq!(original.as_bytes(), &[0xaa, 0x80]);
        drop(converted);
        assert_eq!(allowance.used(), 0);
    }
    assert_eq!(
        from_integer_with_control(-1, 32777, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn bit_string_failures_release_partial_input_and_preserve_cancellation_owners() {
    let mut invalid = "10101010".repeat(4097);
    invalid.pop();
    invalid.push('z');
    let budget = MemoryBudget::new(4097);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        input_with_control(&invalid, BitStringKind::Fixed, -1, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22P02")
    );
    assert_eq!(budget.used(), 0);
    let value = input_with_control(
        "101",
        BitStringKind::Fixed,
        -1,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for result in [
            input_with_control(&invalid, BitStringKind::Fixed, -1, &control),
            coerce_with_control(&value, BitStringKind::Varying, 2, true, &control),
            from_integer_with_control(-1, 33, &control),
        ] {
            assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        }
        assert_eq!(
            output_with_control(&value, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
