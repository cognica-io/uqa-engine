//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn json_object_projection_preserves_sorted_last_values_and_original_source_spans() {
    for text in [
        r"{}",
        r#" { "z":1.00,"a":-0,"huge":1e9999,"none":null } "#,
        r#"{"x":false,"x":[{"nested":"한🙂"}],"\u0078":{},"a":true}"#,
        r#"{"日本語": [1, 2], "quoted\"": "with\\escapes", "": {"a":null,"a":7}}"#,
    ] {
        let expected: serde_json::Value = serde_json::from_str(text).unwrap();
        let expected = expected.as_object().unwrap();
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        let actual = JsonObjectProjection::read(text.as_bytes(), &memory, &cancellation).unwrap();
        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual.is_empty(), expected.is_empty());
        assert_eq!(
            actual.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            expected.keys().map(String::as_str).collect::<Vec<_>>()
        );
        for (name, bytes) in actual.iter() {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(bytes).unwrap(),
                expected[name]
            );
            assert_eq!(actual.get(name).unwrap().as_ptr(), bytes.as_ptr());
            let start = bytes.as_ptr() as usize - text.as_ptr() as usize;
            assert!(start + bytes.len() <= text.len());
            assert_eq!(&text.as_bytes()[start..start + bytes.len()], bytes);
        }
        assert_eq!(actual.get("absent"), None);
        drop(actual);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn json_object_projection_preserves_complete_string_grammar_and_original_depth_limit() {
    for text in [
        r#"{"a":"\ud800"}"#,
        r#"{"a":"\udc00","a":null}"#,
        r#"{"a":{"x":"\ud800"},"a":{}}"#,
        r#"{"a":[1,]}"#,
        r#"{"a":00}"#,
        r#"{"a":null} false"#,
        r#"{"\ud800":null}"#,
    ] {
        assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        assert!(
            JsonObjectProjection::read(text.as_bytes(), &memory, &cancellation).is_err(),
            "{text}"
        );
        assert_eq!(memory.used(), 0);
    }
    for depth in [15, 16, 125, 126, 127, 128, 129] {
        let text = format!(r#"{{"a":{}null{}}}"#, "[".repeat(depth), "]".repeat(depth));
        let expected = serde_json::from_str::<serde_json::Value>(&text);
        let memory = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        let actual = JsonObjectProjection::read(text.as_bytes(), &memory, &cancellation);
        assert_eq!(actual.is_ok(), expected.is_ok(), "depth {depth}");
        drop(actual);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    assert!(JsonObjectProjection::read(b"{\"a\":\"\xff\"}", &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn json_object_projection_borrows_large_values_and_retains_exact_key_and_node_allowances() {
    let text = format!(
        r#"{{"value":"{}","other":0,"value":["{}"]}}"#,
        "x".repeat(1 << 20),
        "y".repeat(4 << 20)
    );
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    let mut actual = None;
    let measured = allocation_counter::measure(|| {
        actual = Some(JsonObjectProjection::read(text.as_bytes(), &memory, &cancellation).unwrap());
    });
    let actual = actual.unwrap();
    assert_eq!(measured.bytes_current, memory.used() as i64);
    assert!(measured.bytes_max < 65536);
    assert_eq!(
        memory.used(),
        actual.fields.allocated_bytes() + actual.fields.keys().map(String::capacity).sum::<usize>()
    );
    assert_eq!(actual.get("value").unwrap().len(), (4 << 20) + 4);
    drop(actual);
    assert_eq!(memory.used(), 0);
}

#[test]
fn json_object_projection_retires_truncation_cancelled_and_denied_names_without_copying_values() {
    let text = r#"{"a":{"b":[null,"한🙂"]},"z":false}"#;
    for end in 0..text.len() {
        let memory = MemoryBudget::new(65536);
        let sibling = memory.reserve(19).unwrap();
        let cancellation = CancellationToken::new();
        assert!(
            JsonObjectProjection::read(&text.as_bytes()[..end], &memory, &cancellation).is_err()
        );
        assert_eq!(memory.used(), sibling.bytes());
    }
    let text = format!(r#"{{"{}":null}}"#, "x".repeat(1 << 20));
    for limit in [1, 63, 255, 4096] {
        let memory = MemoryBudget::new(limit + 19);
        let sibling = memory.reserve(19).unwrap();
        let cancellation = CancellationToken::new();
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                JsonObjectProjection::read(text.as_bytes(), &memory, &cancellation),
                Err(JsonReadError::Memory(_))
            ));
        });
        assert!(measured.bytes_max < 65536);
        assert_eq!(memory.used(), sibling.bytes());
    }
    let memory = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        JsonObjectProjection::read(b"{}", &memory, &cancellation),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

mod resumable;
