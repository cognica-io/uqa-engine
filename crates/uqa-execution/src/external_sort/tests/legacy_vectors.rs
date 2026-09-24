//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{ArrayValue, LegacyVectorKind, LegacyVectorValue};

#[test]
fn legacy_vector_comparison_errors_survive_in_memory_and_spilled_sort() {
    let invalid = Value::LegacyVector(
        LegacyVectorValue::try_from_array(
            LegacyVectorKind::Oid,
            ArrayValue::try_new(Vec::new()).unwrap(),
        )
        .unwrap(),
    );
    for budget in [1, 1_000_000] {
        for nested in [false, true] {
            let key = if nested {
                Value::Array(ArrayValue::try_new(vec![invalid.clone()]).unwrap())
            } else {
                invalid.clone()
            };
            let make_row = |input| {
                BTreeMap::from([
                    ("key".into(), key.clone()),
                    ("input".into(), Value::Int(input)),
                ])
            };
            let mut single = sort(vec![make_row(0)], budget, None);
            assert_eq!(run_to_rows(&mut single).unwrap().1.len(), 1);
            let mut pair = sort(vec![make_row(0), make_row(1)], budget, None);
            let ExecError::SQL(error) = run_to_rows(&mut pair).unwrap_err() else {
                panic!("expected SQL operator failure")
            };
            assert_eq!(error.sqlstate(), Some("42804"));
            assert_eq!(error.to_string(), "array is not a valid oidvector");
        }
    }
}
