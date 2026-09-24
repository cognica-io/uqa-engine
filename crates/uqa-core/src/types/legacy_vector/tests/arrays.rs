//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn array_vector(kind: LegacyVectorKind, values: &[i64], bounds: Vec<i32>) -> LegacyVectorValue {
    LegacyVectorValue::try_from_array(
        kind,
        ArrayValue::with_lower_bounds(values.iter().copied().map(Value::Int).collect(), bounds)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn array_function_bounds_preserve_pg18_equality_and_ordered_keys() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
        let zero = array_vector(kind, &[1], vec![0]);
        let one = array_vector(kind, &[1], vec![1]);
        // PG18 trim_array('1 2'::int2vector,1) retains bound 1 and compares greater than '1'; oidvector operators ignore that bound.
        assert_eq!(zero == one, kind == LegacyVectorKind::Oid);
        assert_eq!(
            zero.cmp(&one),
            if kind == LegacyVectorKind::Oid {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        );
        assert_eq!(zero.compare_as_array(&one), Ordering::Less);
        let values = [
            zero,
            one,
            array_vector(kind, &[1], vec![-1]),
            array_vector(kind, &[1, 2], vec![1]),
            array_vector(kind, &[2], vec![0]),
            array_vector(kind, &[], vec![0]),
            array_vector(kind, &[], vec![]),
        ];
        for left in &values {
            for right in &values {
                assert_eq!(left == right, left.cmp(right).is_eq());
                assert_eq!(key(left).cmp(&key(right)), left.cmp(right));
                assert_eq!(
                    Value::LegacyVector(left.clone())
                        .cmp_with_control(&Value::LegacyVector(right.clone()), &control)
                        .unwrap(),
                    left.cmp(right)
                );
                for last in &values {
                    if left <= right && right <= last {
                        assert!(left <= last);
                    }
                }
            }
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn noncanonical_array_shapes_survive_tagged_decode_and_controlled_copy() {
    for kind in [LegacyVectorKind::SmallInteger, LegacyVectorKind::Oid] {
        for value in [
            array_vector(kind, &[1, 2], vec![1]),
            array_vector(kind, &[], vec![]),
        ] {
            let memory = MemoryBudget::new(1 << 20);
            let cancellation = CancellationToken::new();
            let encoded = serde_json::to_string(&Value::LegacyVector(value.clone())).unwrap();
            let decoded = JsonValueDecoder::new(&memory, &cancellation)
                .value(&encoded)
                .unwrap();
            let copied = decoded.clone_budgeted(&memory, &cancellation).unwrap();
            for carrier in [&*decoded, &*copied] {
                let Value::LegacyVector(vector) = carrier else {
                    panic!("legacy vector kind was lost")
                };
                assert_eq!(vector.kind(), kind);
                assert_eq!(vector.as_array(), value.as_array());
                assert_eq!(
                    vector.has_vector_layout(),
                    !value.as_array().dimensions().is_empty()
                );
                assert_eq!(serde_json::to_string(vector).unwrap(), encoded);
            }
            assert_eq!(
                decoded.reserved_bytes(),
                decoded
                    .retained_payload_bytes(&memory, &cancellation)
                    .unwrap()
            );
            drop((decoded, copied));
            assert_eq!(memory.used(), 0);
        }
    }
}
