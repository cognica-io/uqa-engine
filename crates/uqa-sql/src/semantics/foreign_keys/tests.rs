//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn foreign_key_normalization_preserves_declared_byte_values() {
    let comparison = ForeignKeyComparison {
        comparison_types: vec![ColumnType::InternalChar, ColumnType::BigInteger],
        local_types: vec![ColumnType::InternalChar, ColumnType::SmallInteger],
        referenced_types: vec![ColumnType::InternalChar, ColumnType::BigInteger],
        exact_reference_lookup: true,
        exact_local_lookup: false,
    };
    for byte in 0..=u8::MAX {
        let carrier = crate::expr::internal_char::carrier_with_control(
            byte,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .unwrap()
        .into_uncontrolled()
        .unwrap();
        let values = vec![carrier, Value::Int(42)];
        assert_eq!(comparison.normalize_local(values.clone()).unwrap(), values);
        assert_eq!(
            comparison.normalize_referenced(values.clone()).unwrap(),
            values
        );
    }
    assert!(comparison.normalize_local(vec![Value::Int(42)]).is_err());
}
