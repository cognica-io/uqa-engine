//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

// Bytes captured from PostgreSQL 18's retained composite output after JSONB -> bytea.
const OBJECT_BYTES: &str = "0200002001000080040000001b00005024000050616c6f6e6700000001000020010000800b000010620000002000000000a0030005000040000000c000000030000000200a000010020000002800000000820c00480dc3a9";
const OBJECT_TEXT: &str = r#"{"a": {"b": -3}, "long": [null, true, false, 12.3400, "é"]}"#;

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn jsonb_physical_scalar_and_nested_bytes_match_postgresql() {
    for (text, hex) in [
        (OBJECT_TEXT, OBJECT_BYTES),
        ("123.4500", "010000500a0000902800000000827b009411"),
        (r#""aé""#, "010000500300008061c3a9"),
        ("true", "01000050000000b0"),
        ("null", "01000050000000c0"),
        ("{}", "00000020"),
        ("[]", "00000040"),
    ] {
        let expected = bytes(hex);
        assert_eq!(encode_jsonb_datum(text).unwrap(), expected, "{text}");
        assert_eq!(
            &*decode_jsonb_datum_with_control(&expected, &ProductionControl::uncontrolled())
                .unwrap(),
            text
        );
    }
}

#[test]
fn jsonb_physical_read_preserves_entry_offsets_and_object_order() {
    // Noncanonical key order and offset stride are valid iterator inputs; output must not re-sort.
    let physical = bytes("02000020010000000200008000000030000000207a61");
    assert_eq!(
        &*decode_jsonb_datum_with_control(&physical, &ProductionControl::uncontrolled()).unwrap(),
        r#"{"z": true, "a": false}"#
    );
    let mut malformed = bytes("00000000");
    let error = decode_jsonb_datum_with_control(&malformed, &ProductionControl::uncontrolled())
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "XX000" && message == "unknown type of jsonb container")
    );
    malformed = bytes("0100005000000090");
    assert!(
        decode_jsonb_datum_with_control(&malformed, &ProductionControl::uncontrolled()).is_err()
    );
}

#[test]
fn jsonb_physical_read_admits_output_and_stack_and_releases_every_exit() {
    let physical = bytes(OBJECT_BYTES);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &token, &token);
    let value = decode_jsonb_datum_with_control(&physical, &control).unwrap();
    assert_eq!(&*value, OBJECT_TEXT);
    assert_eq!(memory.used(), value.capacity());
    drop(value);
    assert_eq!(memory.used(), 0);
    for limit in [0, 64, 128] {
        let memory = MemoryBudget::new(limit);
        let control = ProductionControl::new(&memory, &token, &token);
        assert!(decode_jsonb_datum_with_control(&physical, &control).is_err());
        assert_eq!(memory.used(), 0);
    }
    for malformed in [&physical[..7], &physical[..physical.len() - 1]] {
        assert!(decode_jsonb_datum_with_control(malformed, &control).is_err());
        assert_eq!(memory.used(), 0);
    }
    token.cancel();
    assert!(decode_jsonb_datum_with_control(&physical, &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn jsonb_physical_read_uses_an_admitted_stack_for_deep_containers() {
    // A physical container is not constrained by JSON input parser depth.
    let depth = 1024;
    let mut physical = Vec::new();
    for _ in 0..depth {
        physical.extend_from_slice(&(ARRAY | 1).to_le_bytes());
        physical.extend_from_slice(&(CONTAINER | HAS_OFFSET).to_le_bytes());
    }
    physical.extend_from_slice(&ARRAY.to_le_bytes());
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let text = decode_jsonb_datum_with_control(&physical, &control).unwrap();
    assert_eq!(text.len(), (depth + 1) * 2);
    assert_eq!(memory.used(), text.capacity());
    drop(text);
    assert_eq!(memory.used(), 0);
}

#[test]
fn jsonb_physical_comparison_preserves_postgresql_object_entry_order() {
    let payload = bytes("02000020010000000200008000000030000000207a61");
    let mut encoded = ((payload.len() as u32 + 4) << 2).to_le_bytes().to_vec();
    encoded.extend_from_slice(&payload);
    let retained = uqa_core::Value::Datum(uqa_core::DatumValue::new(3802, 0, encoded));
    let fresh = uqa_core::Value::JsonB(r#"{"a": false, "z": true}"#.into());
    assert_eq!(
        crate::expr::binary::compare_typed_values_with_control(
            &retained,
            &fresh,
            &ProductionControl::uncontrolled(),
        )
        .unwrap(),
        std::cmp::Ordering::Greater,
    );
}

#[test]
fn jsonb_physical_comparisons_keep_numeric_order_and_delayed_reads() {
    use crate::ast::BinaryOp;
    use crate::expr::binary::{compare_typed_values_with_control, eval_binary_values_with_control};
    use std::cmp::Ordering::{Equal, Greater, Less};
    use uqa_core::{DatumValue, Value};

    for (hex, text, expected) in [
        (
            "02000020010000800100000000000040000000306161",
            r#"{"a":true}"#,
            Greater,
        ),
        ("01000050060000901800000000c0", "null", Greater),
        ("01000050060000901800000000f0", "-1", Less),
        ("01000050080000902000000000810100", "1", Equal),
        ("00000040", "false", Less),
        ("01000050000000b0", "[]", Greater),
        ("02000040040000d00000004000000000", "[]", Greater),
        ("02000040000000b00400005000000000", "[false, []]", Greater),
    ] {
        let payload = bytes(hex);
        let mut encoded = ((payload.len() as u32 + 4) << 2).to_le_bytes().to_vec();
        encoded.extend_from_slice(&payload);
        let retained = Value::Datum(DatumValue::new(3802, 0, encoded));
        let fresh = Value::JsonB(text.into());
        let memory = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        for control in [ProductionControl::uncontrolled(), control] {
            assert_eq!(
                compare_typed_values_with_control(&retained, &fresh, &control).unwrap(),
                expected
            );
            assert_eq!(
                compare_typed_values_with_control(&fresh, &retained, &control).unwrap(),
                expected.reverse()
            );
            let equal =
                eval_binary_values_with_control(BinaryOp::Equal, &retained, &fresh, &control)
                    .unwrap();
            assert_eq!(*equal, Value::Bool(expected.is_eq()));
            drop(equal);
            assert_eq!(memory.used(), 0);
        }
        let small = MemoryBudget::new(16);
        assert!(compare_typed_values_with_control(
            &retained,
            &fresh,
            &ProductionControl::new(&small, &token, &token)
        )
        .is_err());
        assert_eq!(small.used(), 0);
        token.cancel();
        assert_eq!(
            compare_typed_values_with_control(&retained, &fresh, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn jsonb_physical_comparison_reports_only_reached_invalid_containers() {
    use super::compare::{compare_jsonb_datums_with_control, JsonbInput};
    let physical = bytes("02000040000000b00400005000000000");
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let error = compare_jsonb_datums_with_control(
        JsonbInput::Bytes(&physical),
        JsonbInput::Text("[true, []]"),
        &control,
    )
    .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "XX000" && message == "unknown type of jsonb container")
    );
    assert_eq!(memory.used(), 0);
    let wrong_key = bytes("010000200000003000000020");
    let error = decode_jsonb_datum_with_control(&wrong_key, &control).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "XX000" && message == "unexpected jsonb type as object key")
    );
    assert_eq!(memory.used(), 0);
}
