//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::Value as Json;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn packed_and_uuid_text_inputs_match_independent_postgresql_parameters() {
    let reference: Json = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let control = ProductionControl::uncontrolled();
    for case in reference["cases"].as_array().unwrap() {
        let kind = case["kind"].as_str().unwrap();
        let mut bytes: Vec<_> = case["input_hex"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let input = if kind == "uuid" {
            let bytes = match case["text"].as_str() {
                Some(text) => crate::expr::parse_uuid_bytes_with_control(text, &control),
                None => Ok(bytes.as_slice().try_into().unwrap()),
            };
            bytes.and_then(|bytes| SQLParam::uuid_with_control(bytes, &control))
        } else {
            SQLParam::packed_bits_with_control(
                &bytes,
                u32::try_from(case["bit_length"].as_u64().unwrap()).unwrap(),
                if kind == "bit" {
                    BitStringKind::Fixed
                } else {
                    BitStringKind::Varying
                },
                &control,
            )
        };
        bytes.fill(0x5a);
        let expected_state = case["sqlstate"].as_str().unwrap();
        if !expected_state.is_empty() {
            let error = input.unwrap_err();
            assert_eq!(error.sqlstate(), Some(expected_state), "{case}");
            let SQLError::Routine { message, .. } = error.primary() else {
                panic!("{error}");
            };
            assert_eq!(message, case["message"].as_str().unwrap(), "{case}");
            continue;
        }
        let input = input.unwrap();
        let ty = input.declared_scalar_type().unwrap();
        let expected_type = match kind {
            "uuid" => ColumnType::Uuid,
            "bit" => ColumnType::Bit(None),
            "varbit" => ColumnType::VarBit(None),
            _ => unreachable!(),
        };
        assert_eq!(ty, &expected_type);
        let text =
            crate::result::format_postgres_text(input.scalar_value().unwrap(), ty, None).unwrap();
        assert_eq!(text, case["rows"][0][0].as_str().unwrap(), "{case}");
    }
}

#[test]
fn packed_parameters_retain_only_the_original_allowance_and_cancel_both_owners() {
    let budget = MemoryBudget::new(128);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    for kind in 0..3 {
        let create = || match kind {
            0 => SQLParam::uuid_with_control([0xff; 16], &control),
            1 => SQLParam::packed_bits_with_control(&[0xff; 4], 27, BitStringKind::Fixed, &control),
            _ => {
                SQLParam::packed_bits_with_control(&[0xff; 4], 27, BitStringKind::Varying, &control)
            }
        };
        let input = create().unwrap();
        assert!(input.reserved_bytes() >= 4);
        assert_eq!(
            budget.used(),
            held.reserved_bytes() + input.reserved_bytes()
        );
        drop(input);
        let occupied = control.copy_text(&"x".repeat(112)).unwrap();
        assert_eq!(create().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(
            budget.used(),
            held.reserved_bytes() + occupied.reserved_bytes()
        );
        drop(occupied);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(create().unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(budget.used(), held.reserved_bytes());
            token.reset();
        }
        drop(create().unwrap());
        assert_eq!(budget.used(), held.reserved_bytes());
    }
}

#[test]
fn malformed_packed_lengths_do_not_admit_or_retain_partial_input() {
    let budget = MemoryBudget::new(32);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    for (bytes, length) in [
        (&[][..], u32::MAX),
        (&[][..], 1),
        (&[0xff][..], 0),
        (&[0xff][..], 9),
    ] {
        let error =
            SQLParam::packed_bits_with_control(bytes, length, BitStringKind::Fixed, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P03"));
        assert_eq!(budget.used(), held.reserved_bytes());
    }
    let bytes = [0xff; 4096];
    assert_eq!(
        SQLParam::packed_bits_with_control(&bytes, 32768, BitStringKind::Fixed, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), held.reserved_bytes());
}
