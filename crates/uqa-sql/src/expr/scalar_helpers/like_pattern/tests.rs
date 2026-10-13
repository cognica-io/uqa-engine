//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CompiledLikePattern, LikePatternToken};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn binary_like_borrows_bytes_and_preserves_padding_without_text_conversion() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let pattern = CompiledLikePattern::from_value_with_escape_with_control(
        &Value::Bytes(vec![b'_', b'%']),
        false,
        None,
        &control,
    )
    .unwrap();
    let retained = memory.used();
    assert!(pattern.try_is_match_with_control("é", &control).unwrap());
    assert_eq!(memory.used(), retained);
    let source = Value::Bytes(vec![0xff; 32 * 1024]);
    assert!(pattern
        .try_matches_value_with_control(&source, &control)
        .unwrap());
    assert_eq!(memory.used(), retained);
    drop(pattern);
    assert_eq!(memory.used(), 0);
    let pattern =
        CompiledLikePattern::with_escape_with_control("a", false, None, &control).unwrap();
    assert!(!pattern
        .try_matches_value_with_control(&Value::FixedChar("a   ".into()), &control)
        .unwrap());
    drop(pattern);
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_like_keeps_native_pattern_capacity_until_last_owner_release() {
    let memory = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let pattern =
        CompiledLikePattern::with_escape_with_control("a%_", false, None, &control).unwrap();
    let native_capacity = pattern.pattern_chars.capacity() * size_of::<LikePatternToken<char>>()
        + pattern.pattern_ascii.as_ref().unwrap().capacity() * size_of::<LikePatternToken<u8>>();
    assert_eq!(memory.used(), native_capacity);
    assert!(pattern.try_is_match_with_control("a中é", &control).unwrap());
    assert_eq!(memory.used(), native_capacity);
    assert!(pattern
        .try_is_match_with_control("abcdef", &control)
        .unwrap());
    assert_eq!(memory.used(), native_capacity);
    invoking.cancel();
    assert_eq!(
        pattern
            .try_is_match_with_control("a中é", &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), native_capacity);
    drop(pattern);
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_like_quota_failure_drops_partial_tokens_and_match_scratch() {
    let memory = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let pattern = CompiledLikePattern::with_escape_with_control("%", true, None, &control).unwrap();
    let retained = memory.used();
    let error = pattern
        .try_is_match_with_control(&"É".repeat(1024), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), retained);
    let failed =
        CompiledLikePattern::with_escape_with_control(&"a".repeat(1024), false, None, &control);
    assert!(failed.is_err());
    assert_eq!(failed.err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), retained);
    drop(pattern);
    assert_eq!(memory.used(), 0);
}
