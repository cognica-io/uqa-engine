//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{LegacyVectorKind, LegacyVectorValue};

#[test]
fn catalog_vector_casts_preserve_element_identity_bounds_and_admission() {
    let catalog = Catalog::default();
    let budget = MemoryBudget::new(64 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let target = ColumnType::Array(Box::new(ColumnType::Regtype));
    for kind in [LegacyVectorKind::Oid, LegacyVectorKind::SmallInteger] {
        for elements in [vec![Value::Int(23), Value::Int(25)], Vec::new()] {
            let input =
                Value::LegacyVector(LegacyVectorValue::try_new(kind, elements.clone()).unwrap());
            let output =
                cast_catalog_array(&input, Some(kind.type_name()), &target, &catalog, &control)
                    .unwrap();
            let Value::Array(array) = &*output else {
                panic!("catalog array");
            };
            assert_eq!(array.elements(), elements);
            assert_eq!(array.element_type_oid(), Some(2206));
            if elements.is_empty() && kind == LegacyVectorKind::SmallInteger {
                assert!(array.dimensions().is_empty());
                assert!(array.lower_bounds().is_empty());
            } else {
                assert_eq!(array.dimensions(), [elements.len()]);
                assert_eq!(array.lower_bounds(), [0]);
            }
            assert_eq!(budget.used(), output.reserved_bytes());
            let occupied = budget.reserve(budget.limit() - budget.used()).unwrap();
            let failure =
                cast_catalog_array(&input, Some(kind.type_name()), &target, &catalog, &control)
                    .unwrap_err();
            assert_eq!(failure.sqlstate(), Some("53200"));
            drop(occupied);
            assert_eq!(budget.used(), output.reserved_bytes());
            assert_eq!(
                input,
                Value::LegacyVector(LegacyVectorValue::try_new(kind, elements).unwrap())
            );
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
    for token in [&original, &invoking] {
        token.cancel();
        let input = Value::LegacyVector(
            LegacyVectorValue::try_new(LegacyVectorKind::Oid, vec![Value::Int(23)]).unwrap(),
        );
        let failure =
            cast_catalog_array(&input, Some("oidvector"), &target, &catalog, &control).unwrap_err();
        assert_eq!(failure.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
