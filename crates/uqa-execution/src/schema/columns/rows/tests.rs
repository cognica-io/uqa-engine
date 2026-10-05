//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use uqa_core::{ArrayValue, EnumLabelKey, EnumValue};

#[test]
fn original_and_rewritten_rows_share_one_allowance_and_keep_typed_values() {
    for limit in [0, 128, 1 << 16] {
        let memory = MemoryBudget::new(limit);
        let mut original = RewriteRows::new(&memory);
        let mut converted = RewriteRows::new(&memory);
        let document = BTreeMap::from([
            (
                "enum".into(),
                Value::Enum(EnumValue::new(
                    16_384,
                    EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
                )),
            ),
            (
                "array".into(),
                Value::Array(
                    ArrayValue::with_lower_bounds(vec![Value::Null, Value::Float(-0.0)], vec![-2])
                        .unwrap(),
                ),
            ),
            ("payload".into(), Value::Str("row".repeat(256))),
        ]);
        for index in 0..20 {
            original.push(DocId::MAX - index, document.clone()).unwrap();
            let row = original.get(index).unwrap();
            converted
                .push_replacement(row.original_id, index, row.document)
                .unwrap();
            assert!(memory.used() <= limit);
        }
        assert!(memory.peak() <= limit);
        drop(original);
        for index in (0..20).rev() {
            let row = converted.get(index).unwrap();
            assert_eq!(row.original_id, DocId::MAX - index);
            assert_eq!(row.target_id, index);
            assert_eq!(row.document, document);
            let Value::Array(array) = &row.document["array"] else {
                panic!("array carrier");
            };
            assert_eq!(array.lower_bounds(), [-2]);
            let Value::Float(value) = array.elements()[1] else {
                panic!("float carrier");
            };
            assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
        }
        converted.spill().unwrap();
        assert_eq!(memory.used(), 0);
        assert_eq!(converted.get(0).unwrap().document, document);
        drop(converted);
        assert_eq!(memory.used(), 0);
    }
}
