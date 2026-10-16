//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;
use uqa_core::LegacyVectorKind;

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../binary/comparison/image/postgresql.json"
    ))
    .unwrap()
}

fn payload(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect()
}

fn datum(ty: &ColumnType, payload: &[u8]) -> DatumValue {
    let mut bytes = vec![0xff];
    bytes.extend_from_slice(&(((payload.len() + 4) as u32) << 2).to_le_bytes());
    bytes.extend_from_slice(payload);
    DatumValue::new(
        crate::catalog::type_metadata::pg_type_oid(ty) as u32,
        1,
        bytes,
    )
}

#[test]
fn physical_legacy_vector_reads_preserve_postgresql_values_and_output_leases() {
    let fixture = reference();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8192);
    let control = ProductionControl::new(&memory, &token, &token);
    for case in fixture["cases"].as_array().unwrap() {
        let (ty, kind) = match case["type"].as_str().unwrap() {
            "int2vector" => (ColumnType::Int2Vector, LegacyVectorKind::SmallInteger),
            "oidvector" => (ColumnType::OidVector, LegacyVectorKind::Oid),
            _ => continue,
        };
        for (field, image) in [("left", "left_payload_hex"), ("right", "right_payload_hex")] {
            let bytes = payload(case[image].as_str().unwrap());
            let value = read_with_control(&datum(&ty, &bytes), &control).unwrap();
            let Value::LegacyVector(vector) = &*value else {
                panic!("selected vector carrier");
            };
            let text = case[field].as_str().unwrap();
            let expected = text
                .split_whitespace()
                .map(|word| Value::Int(word.parse().unwrap()))
                .collect::<Vec<_>>();
            assert_eq!(vector.kind(), kind);
            assert_eq!(vector.elements(), expected);
            assert_eq!(vector.as_array().dimensions(), [expected.len()]);
            assert_eq!(vector.as_array().lower_bounds(), [0]);
            assert_eq!(
                crate::result::format_postgres_text(&value, &ty, None).unwrap(),
                text
            );
            assert_eq!(memory.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn physical_legacy_vector_read_errors_release_memory_and_preserve_cancellation() {
    let fixture = reference();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8192);
    let control = ProductionControl::new(&memory, &token, &token);
    for reference in fixture["vector_read_errors"].as_array().unwrap() {
        let source = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["type"] == reference["physical_type"])
            .unwrap();
        let bytes = payload(source["left_payload_hex"].as_str().unwrap());
        let ty = ColumnType::from_sql_name(reference["selected_type"].as_str().unwrap()).unwrap();
        let error = read_with_control(&datum(&ty, &bytes), &control)
            .map(drop)
            .unwrap_err();
        assert_eq!(error.sqlstate(), reference["sqlstate"].as_str());
        assert_eq!(error.to_string(), reference["message"].as_str().unwrap());
        assert_eq!(memory.used(), 0);
    }
    for name in ["int2vector", "oidvector"] {
        let source = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["type"] == name && case["left"] != "")
            .unwrap();
        let bytes = payload(source["left_payload_hex"].as_str().unwrap());
        let ty = ColumnType::from_sql_name(name).unwrap();
        for length in 0..bytes.len() {
            let error = read_with_control(&datum(&ty, &bytes[..length]), &control)
                .map(drop)
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX001"), "{name}: {length}");
            assert_eq!(memory.used(), 0);
        }
        let exhausted = MemoryBudget::new(0);
        let error = read_with_control(
            &datum(&ty, &bytes),
            &ProductionControl::new(&exhausted, &token, &token),
        )
        .map(drop)
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(exhausted.used(), 0);
        for original_cancelled in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&memory, &original, &invoking);
            let error = read_with_control(&datum(&ty, &bytes), &control)
                .map(drop)
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
    }
}
