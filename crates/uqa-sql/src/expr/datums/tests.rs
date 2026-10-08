//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn compressed(method: u32, length: u32, bytes: &[u8]) -> DatumValue {
    let mut data = (((bytes.len() as u32 + 8) << 2) | 2).to_le_bytes().to_vec();
    data.extend_from_slice(&((method << 30) | length).to_le_bytes());
    data.extend_from_slice(bytes);
    DatumValue::new(25, 0, data)
}

#[test]
fn physical_scalar_headers_preserve_short_and_uncompressed_payloads() {
    for (oid, expected) in [
        (25, Value::Str("aé".into())),
        (17, Value::Bytes("aé".as_bytes().to_vec())),
    ] {
        let mut short = vec![9];
        short.extend_from_slice("aé".as_bytes());
        let mut long = (7_u32 << 2).to_le_bytes().to_vec();
        long.extend_from_slice("aé".as_bytes());
        for bytes in [short, long] {
            assert_eq!(read(&DatumValue::new(oid, 0, bytes)).unwrap(), expected);
        }
    }
}

#[test]
fn physical_scalar_compression_matches_pg_formats_and_rejects_trailing_or_truncated_bytes() {
    // PG's PGLZ control bits are least-significant first: three literals, then a six-byte backreference at distance three. LZ4 encodes the same nine literal bytes with token 0x90.
    let pglz = [8, b'a', b'b', b'c', 3, 3];
    let lz4 = [0x90, b'a', b'b', b'c', b'a', b'b', b'c', b'a', b'b', b'c'];
    for (method, bytes) in [(0, &pglz[..]), (1, &lz4[..])] {
        assert_eq!(
            read(&compressed(method, 9, bytes)).unwrap(),
            Value::Str("abcabcabc".into())
        );
        for end in 0..bytes.len() {
            let error = read(&compressed(method, 9, &bytes[..end])).unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX001"));
            assert_eq!(
                error.to_string(),
                format!(
                    "compressed {} data is corrupt",
                    if method == 0 { "pglz" } else { "lz4" }
                )
            );
        }
        let mut trailing = bytes.to_vec();
        trailing.push(0);
        assert_eq!(
            read(&compressed(method, 9, &trailing))
                .unwrap_err()
                .sqlstate(),
            Some("XX001")
        );
    }
}

#[test]
fn physical_scalar_decompression_is_admitted_and_cancellable_before_output() {
    let datum = compressed(0, 9, &[8, b'a', b'b', b'c', 3, 3]);
    let memory = MemoryBudget::new(8);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert!(read_with_control(&datum, &control).is_err());
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(128);
    let control = ProductionControl::new(&memory, &token, &token);
    let output = read_with_control(&datum, &control).unwrap();
    assert_eq!(*output, Value::Str("abcabcabc".into()));
    assert!(memory.used() >= 9);
    drop(output);
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert_eq!(
        read_with_control(&datum, &control).unwrap_err().sqlstate(),
        Some("57014")
    );
}

#[test]
fn numeric_datum_output_admits_expansion_and_retains_only_the_result() {
    let number = DecimalValue::parse("-12345.678900").unwrap();
    let encoded = crate::catalog::node_tree::encode_numeric_datum(&number).unwrap();
    let mut bytes = vec![((encoded.len() as u8 + 1) << 1) | 1];
    bytes.extend_from_slice(&encoded);
    let datum = DatumValue::new(1700, 0, bytes);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let output = read_with_control(&datum, &control).unwrap();
    assert_eq!(*output, Value::Decimal(number));
    assert!(memory.used() > 0);
    drop(output);
    assert_eq!(memory.used(), 0);

    // A small physical numeric can request thousands of leading decimal groups.
    let large = DatumValue::new(1700, 0, vec![15, 0, 0, 255, 127, 1, 0]);
    let memory = MemoryBudget::new(128);
    let control = ProductionControl::new(&memory, &token, &token);
    assert!(read_with_control(&large, &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn physical_scalar_reads_defer_corruption_until_a_consuming_comparison() {
    let bad = Value::Datum(DatumValue::new(1700, 0, vec![2, 0, 0, 0, 3, b'x']));
    let control = ProductionControl::uncontrolled();
    assert!(!uqa_core::sql_null_test(Some(&bad), false));
    assert!(uqa_core::sql_null_test(Some(&bad), true));
    assert_eq!(
        crate::expr::values_equal_nullable_with_control(&bad, &Value::Null, &control).unwrap(),
        None
    );
    let left = Value::Record(vec![("a".into(), Value::Int(1)), ("b".into(), bad.clone())]);
    let right = Value::Record(vec![("a".into(), Value::Int(2)), ("b".into(), bad.clone())]);
    assert!(
        crate::expr::compare_typed_values_with_control(&left, &right, &control)
            .unwrap()
            .is_lt()
    );
    let error = crate::expr::compare_typed_values_with_control(&left, &left, &control).unwrap_err();
    assert_eq!(error.to_string(), "compressed pglz data is corrupt");
}
