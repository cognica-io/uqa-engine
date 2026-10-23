//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, Value};
use std::collections::BTreeMap;

#[test]
fn visited_root_fields_borrow_names_and_release_discarded_payloads() {
    let name = "field".repeat(8192);
    let input = format!(r#"{{"{name}":1,"\u0061":2,"a":3}}"#);
    let memory = MemoryBudget::new(2048);
    let cancellation = CancellationToken::new();
    let mut seen = 0;
    let allocated = allocation_counter::measure(|| {
        JsonValueDecoder::new(&memory, &cancellation)
            .visit_fields(&input, &mut |key, value| {
                if seen == 0 {
                    assert_eq!(key, name);
                    assert!(input.as_bytes().as_ptr_range().contains(&key.as_ptr()));
                } else {
                    assert_eq!(key, "a");
                }
                seen += 1;
                assert_eq!(*value, Value::Int(seen));
                Ok(())
            })
            .unwrap();
    });
    assert_eq!(seen, 3);
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    assert_eq!(memory.used(), 0);
}

#[test]
fn visited_root_fields_keep_conversion_order_and_duplicate_semantics() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let decoder = JsonValueDecoder::new(&memory, &cancellation);
    for input in [
        r#"{"a":[1,2],"b":{"$uqa_type":"row","values":[1,2]},"\u0061":null}"#,
        r#"{"$uqa_type":"bytes","hex":"ff","nested":{"$uqa_type":"bytes","hex":"ff"}}"#,
        r#"{"a":{"$serde_json::private::Number":"bad"},"a":1}"#,
        r#"{"a":{"$serde_json::private::Number":"bad"},"b":?}"#,
        r#"{"a":1,"b":01}"#,
        r#"{"a":"\uD800"}"#,
        r#"{"a":1} false"#,
        "[]",
        "null",
        "{}",
    ] {
        let expected = decoder.fields(input);
        let mut collected = BTreeMap::new();
        let actual = decoder.visit_fields(input, &mut |name, value| {
            collected.insert(name.to_owned(), value);
            Ok(())
        });
        match (expected, actual) {
            (Ok(expected), Ok(())) => {
                assert_eq!(expected.len(), collected.len());
                for (name, value) in &collected {
                    assert_eq!(expected.get(name), Some(&**value), "{input}");
                }
            }
            (Err(expected), Err(actual)) => assert_eq!(expected.to_string(), actual.to_string()),
            (expected, actual) => panic!("{input}: {expected:?} / {actual:?}"),
        }
        drop(collected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn visited_root_cancellation_and_quota_preserve_unrelated_leases() {
    let memory = MemoryBudget::new(2048);
    let cancellation = CancellationToken::new();
    let decoder = JsonValueDecoder::new(&memory, &cancellation);
    let unrelated = memory.reserve(7).unwrap();
    assert!(matches!(
        decoder.visit_fields(r#"{"a":1}"#, &mut |_, _| {
            cancellation.cancel();
            Ok(())
        }),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 7);
    cancellation.reset();
    let input = format!(r#"{{"a":"{}"}}"#, "x".repeat(4096));
    assert!(matches!(
        decoder.visit_fields(&input, &mut |_, _| panic!("unadmitted value")),
        Err(JsonReadError::Memory(_))
    ));
    assert_eq!(memory.used(), 7);
    drop(unrelated);
}
