//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSONB ranges use native structural ordering, independent of physical JSON text spelling.

use super::*;

#[test]
fn jsonb_index_keys_preserve_native_ranges_and_semantic_equality() {
    let mut values = vec![Value::Null];
    values.extend(
        [
            "null",
            "\"a\"",
            "\"a\\u0000\"",
            "0",
            "0.1",
            "-1.01",
            "1",
            "1.00",
            "123456789012345678901234567890.1",
            "false",
            "true",
            "[]",
            "[1]",
            "[2]",
            "[1,0]",
            "{}",
            "{\"a\":1,\"b\":[2]}",
            "{\"b\":[2.0],\"a\":1.00}",
            "{\"b\":1,\"zz\":1}",
            "{\"c\":1,\"aa\":1}",
        ]
        .map(|text| Value::JsonB(text.into())),
    );
    let mut targets = values.clone();
    targets.extend([
        Value::Str("null".into()),
        Value::Json("null".into()),
        Value::List(Vec::new()),
    ]);
    verify(ScalarIndexDomain::JsonBinary, &values, &targets);
    for (low, high) in [
        ("[1]", "[3]"),
        ("[]", "false"),
        ("{\"b\":1,\"zz\":1}", "{\"c\":1,\"aa\":1}"),
    ] {
        let predicate = Predicate::Between {
            low: Value::JsonB(low.into()),
            high: Value::JsonB(high.into()),
        };
        for value in &values {
            assert_eq!(
                observed(ScalarIndexDomain::JsonBinary, &predicate, value),
                predicate.evaluate(Some(value))
            );
        }
    }
    let alias = ColumnType::Domain {
        schema: "public".into(),
        name: "payload".into(),
        oid: 42002,
        base: Box::new(ColumnType::JsonB),
    };
    assert_eq!(
        ScalarIndexDomain::from_column_type(&alias),
        Some(ScalarIndexDomain::JsonBinary)
    );
}

#[test]
fn jsonb_index_parsing_uses_original_allowance_and_typed_errors() {
    let control = StorageReadControl::with_limit(64);
    let value = Value::JsonB("{\"a\":[1,2,3]}".into());
    let error = ScalarIndexDomain::JsonBinary
        .encode(&value, &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let error = ScalarIndexDomain::JsonBinary
        .encode(&value, &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(control.memory().used(), 0);
    let control = StorageReadControl::with_limit(4096);
    let error = ScalarIndexDomain::JsonBinary
        .encode(&Value::JsonB("{broken".into()), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(control.memory().used(), 0);
    let key = ScalarIndexDomain::JsonBinary
        .encode(&value, &control)
        .unwrap();
    assert!(key.budget().shares_allowance(control.memory()));
    assert_eq!(control.memory().used(), key.capacity());
}

#[test]
fn jsonb_index_ranges_and_hash_keys_match_postgresql() {
    use crate::distinct::{canonical_row_key, canonical_row_key_budgeted, hash_canonical_row};
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-core/src/types/tests/pg18_jsonb.json"
    )))
    .unwrap();
    let values: Vec<_> = reference["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|text| Value::JsonB(text.as_str().unwrap().into()))
        .collect();
    let control = StorageReadControl::with_limit(64 * 1024);
    let hasher = std::collections::hash_map::RandomState::new();
    for pair in reference["comparisons"].as_array().unwrap() {
        let left = &values[pair[0].as_u64().unwrap() as usize];
        let right = &values[pair[1].as_u64().unwrap() as usize];
        let equal = pair[2] == true;
        let expected = if equal {
            std::cmp::Ordering::Equal
        } else if pair[3] == true {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
        let domain = ScalarIndexDomain::JsonBinary;
        assert_eq!(
            domain
                .encode(left, &control)
                .unwrap()
                .as_ref()
                .cmp(domain.encode(right, &control).unwrap().as_ref()),
            expected
        );
        assert_eq!(
            observed(domain, &Predicate::LessThan(right.clone()), left),
            pair[3] == true
        );
        assert_eq!(
            observed(domain, &Predicate::GreaterThan(right.clone()), left),
            pair[4] == true
        );
        let key = canonical_row_key(std::slice::from_ref(left)).unwrap();
        assert_eq!(
            key == canonical_row_key(std::slice::from_ref(right)).unwrap(),
            equal
        );
        let budgeted = canonical_row_key_budgeted(std::iter::once(Some(left)), &control).unwrap();
        assert_eq!(&*budgeted, key);
        drop(budgeted);
        if equal {
            assert_eq!(
                hash_canonical_row(&hasher, std::iter::once(Some(left))).unwrap(),
                hash_canonical_row(&hasher, std::iter::once(Some(right))).unwrap()
            );
        }
        assert_eq!(control.memory().used(), 0);
    }
}
