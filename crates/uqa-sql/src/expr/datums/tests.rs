//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn constant_field_copy_observes_compression_before_scalar_consumers() {
    let malformed = Value::Datum(DatumValue::new(17, 0, vec![2, 0, 0, 0, 3, 0, 0, 0]));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(binary_length(&malformed, &control).unwrap(), Some(3));
    for ty in [crate::ColumnType::Bytea, crate::ColumnType::Text] {
        let error = copy_constant_field(&malformed, &ty).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX001"));
        assert_eq!(error.to_string(), "compressed pglz data is corrupt");
    }
    let jsonb = Value::Datum(DatumValue::new(3802, 0, vec![11, 42, 0, 0, 0]));
    let Value::Datum(copied) = copy_constant_field(&jsonb, &crate::ColumnType::JsonB).unwrap()
    else {
        panic!("constant copy must preserve physical scalar contents")
    };
    assert_eq!(copied.bytes(), &[32, 0, 0, 0, 42, 0, 0, 0]);
    assert_eq!(copied.offset(), 0);
    assert_eq!(
        read(&copied).unwrap_err().to_string(),
        "unknown type of jsonb container"
    );
    for (start, length, expected) in [(-2, 1, None), (1, -1, Some("22011"))] {
        let result = crate::expr::scalar_dispatch::eval_generated_scalar_function(
            "substr",
            &[malformed.clone(), Value::Int(start), Value::Int(length)],
            &control,
        );
        match expected {
            Some(sqlstate) => assert_eq!(result.unwrap_err().sqlstate(), Some(sqlstate)),
            None => assert_eq!(*result.unwrap(), Value::Bytes(Vec::new())),
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn binary_consumers_borrow_physical_payload_and_retain_only_their_output() {
    use crate::expr::scalar_dispatch::eval_generated_scalar_function as eval;
    let value = Value::Datum(DatumValue::new(17, 0, vec![11, 0x41, 0xc3, 0xa9, 0x7a]));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &token, &token);
    for (name, args, expected) in [
        (
            "substr",
            vec![value.clone(), Value::Int(2), Value::Int(2)],
            Value::Bytes(vec![0xc3, 0xa9]),
        ),
        (
            "substr",
            vec![value.clone(), Value::Int(0), Value::Int(2)],
            Value::Bytes(vec![0x41]),
        ),
        (
            "substr",
            vec![value.clone(), Value::Int(8)],
            Value::Bytes(vec![]),
        ),
        (
            "reverse",
            vec![value.clone()],
            Value::Bytes(vec![0x7a, 0xa9, 0xc3, 0x41]),
        ),
        (
            "encode",
            vec![value.clone(), Value::Str("hex".into())],
            Value::Str("41c3a97a".into()),
        ),
        (
            "md5",
            vec![value.clone()],
            Value::Str("71ea09eff300c4da2ef1c65cfca7525a".into()),
        ),
        (
            "concat_op",
            vec![value.clone(), Value::Bytes(vec![0])],
            Value::Bytes(vec![0x41, 0xc3, 0xa9, 0x7a, 0]),
        ),
        (
            "strpos",
            vec![value.clone(), Value::Bytes(vec![0xc3, 0xa9])],
            Value::Int(2),
        ),
        (
            "strpos",
            vec![value.clone(), Value::Bytes(vec![])],
            Value::Int(1),
        ),
    ] {
        let output = eval(name, &args, &control).unwrap();
        assert_eq!(*output, expected, "{name}");
        assert_eq!(memory.used(), output.reserved_bytes(), "{name}");
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    let zero = MemoryBudget::new(0);
    let control = ProductionControl::new(&zero, &token, &token);
    for (name, args, expected) in [
        ("length", vec![value.clone()], 4),
        ("octet_length", vec![value.clone()], 4),
        ("bit_length", vec![value.clone()], 32),
        ("crc32", vec![value.clone()], 1_578_129_375),
        ("crc32c", vec![value.clone()], 2_288_826_224),
        ("get_byte", vec![value.clone(), Value::Int(1)], 195),
    ] {
        assert_eq!(
            *eval(name, &args, &control).unwrap(),
            Value::Int(expected),
            "{name}"
        );
        assert_eq!(zero.used(), 0);
    }
    assert!(eval("reverse", std::slice::from_ref(&value), &control).is_err());
    assert_eq!(zero.used(), 0);
    let invalid = Value::Datum(DatumValue::new(17, 0, Vec::new()));
    assert_eq!(
        *eval("substr", &[invalid, Value::Null], &control).unwrap(),
        Value::Null
    );
    token.cancel();
    assert_eq!(
        eval("length", &[value], &control).unwrap_err().sqlstate(),
        Some("57014")
    );
}

#[test]
fn retained_array_header_controls_elements_bounds_and_resource_ownership() {
    // Captured by interpreting PostgreSQL's retained integer[][] field as bytea; the payload includes its null bitmap and non-default bounds.
    let hex = "0200000028000000170000000200000002000000ffffffff020000000d00000000000000010000000300000004000000";
    let payload = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let mut bytes = ((payload.len() as u32 + 4) << 2).to_le_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    let datum = DatumValue::new(1009, 0, bytes);
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = read_with_control(&datum, &control).unwrap();
    let Value::Array(array) = &*value else {
        panic!("array expected");
    };
    assert_eq!(array.element_type_oid(), Some(23));
    assert_eq!(array.dimensions(), &[2, 2]);
    assert_eq!(array.lower_bounds(), &[-1, 2]);
    assert_eq!(
        array.elements(),
        &[
            Value::List(vec![Value::Int(1), Value::Null]),
            Value::List(vec![Value::Int(3), Value::Int(4)])
        ]
    );
    assert!(memory.used() > 0);
    drop(value);
    assert_eq!(memory.used(), 0);
    for end in 0..datum.bytes().len() {
        assert!(read(&DatumValue::new(1009, 0, datum.bytes()[..end].to_vec())).is_err());
    }
    let small = MemoryBudget::new(64);
    assert!(read_with_control(&datum, &ProductionControl::new(&small, &token, &token)).is_err());
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        read_with_control(&datum, &control).unwrap_err().sqlstate(),
        Some("57014")
    );
}

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
        (114, Value::Json("aé".into())),
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
fn retained_json_payload_is_not_revalidated_by_type_output() {
    let retained = Value::Datum(DatumValue::new(
        114,
        0,
        vec![13, b'h', b'e', b'l', b'l', b'o'],
    ));
    let record = Value::Record(vec![("a".into(), retained), ("b".into(), Value::Int(7))]);
    assert_eq!(
        crate::expr::eval_scalar_function("to_json", &[record]).unwrap(),
        Value::Json("{\"a\":hello,\"b\":7}".into())
    );
}

#[test]
fn fixed_reference_outputs_retain_bounds_and_the_output_allowance() {
    let bytes = vec![1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0];
    let datum = DatumValue::new(2950, 0, bytes);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let output = read_with_control(&datum, &control).unwrap();
    assert_eq!(
        *output,
        Value::Str("01000000-0000-0000-0200-000003000000".into())
    );
    assert!(memory.used() >= 36);
    drop(output);
    assert_eq!(memory.used(), 0);
    let interval = read_with_control(&datum.field(1186, 0), &control).unwrap();
    assert_eq!(
        *interval,
        Value::Temporal(uqa_core::TemporalValue::Interval {
            months: 3,
            days: 2,
            micros: 1
        })
    );
    drop(interval);
    assert_eq!(
        read(&datum.field(1186, 0)).unwrap(),
        Value::Temporal(uqa_core::TemporalValue::Interval {
            months: 3,
            days: 2,
            micros: 1,
        })
    );
    assert_eq!(
        read(&DatumValue::new(19, 1, b"xABCD\0tail".to_vec())).unwrap(),
        Value::Str("ABCD".into())
    );
    for length in 0..16 {
        assert!(read(&DatumValue::new(2950, 0, datum.bytes()[..length].to_vec())).is_err());
    }
    let small = MemoryBudget::new(8);
    assert!(read_with_control(&datum, &ProductionControl::new(&small, &token, &token)).is_err());
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        read_with_control(&datum, &control).unwrap_err().sqlstate(),
        Some("57014")
    );
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
