//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn canonical_json_reuses_string_encoding_workspace_across_native_fields() {
    let value = Value::Array(vec![Value::String("word".into()); 4096]);
    let expected = serde_json::to_vec(&value).unwrap();
    let mut actual = Vec::new();
    let allocated = allocation_counter::measure(|| {
        crate::work::inline(write_value::<serde_json::Error>(
            &value,
            &mut actual,
            &mut (),
        ))
        .unwrap();
    });
    assert_eq!(actual, expected);
    assert!(
        allocated.bytes_total <= u64::try_from(actual.capacity() * 2 + 4096).unwrap(),
        "string encoding retained {} allocated bytes for {} output capacity",
        allocated.bytes_total,
        actual.capacity()
    );
}

#[test]
fn canonical_json_borrows_ordered_objects_and_sorts_unordered_members() {
    let mut values = serde_json::Map::new();
    for index in 0..4096 {
        values.insert(format!("{index:04}"), Value::String("word".into()));
    }
    let value = Value::Object(values);
    let expected = serde_json::to_vec(&value).unwrap();
    let mut actual = Vec::new();
    let allocated = allocation_counter::measure(|| {
        crate::work::inline(write_value::<serde_json::Error>(
            &value,
            &mut actual,
            &mut (),
        ))
        .unwrap();
    });
    assert_eq!(actual, expected);
    assert!(allocated.bytes_total <= u64::try_from(actual.capacity() * 2 + 4096).unwrap());

    let mut reversed = serde_json::Map::new();
    for index in (0..4096).rev() {
        reversed.insert(format!("{index:04}"), Value::String("word".into()));
    }
    let mut actual = Vec::new();
    crate::work::inline(write_value::<serde_json::Error>(
        &Value::Object(reversed),
        &mut actual,
        &mut (),
    ))
    .unwrap();
    assert_eq!(actual, expected);
}
