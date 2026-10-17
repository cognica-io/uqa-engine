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

const DEFAULT_COLLATION: u32 = crate::catalog::collations::DEFAULT_COLLATION_OID as u32;

#[test]
fn binary_like_borrows_bytes_and_preserves_padding_without_text_conversion() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let pattern = CompiledLikePattern::from_value_with_escape_with_control(
        &Value::Bytes(vec![b'_', b'%']),
        false,
        None,
        DEFAULT_COLLATION,
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
    let pattern = CompiledLikePattern::with_escape_with_control(
        "a",
        false,
        None,
        DEFAULT_COLLATION,
        &control,
    )
    .unwrap();
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
    let pattern = CompiledLikePattern::with_escape_with_control(
        "a%_",
        false,
        None,
        DEFAULT_COLLATION,
        &control,
    )
    .unwrap();
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
    let pattern =
        CompiledLikePattern::with_escape_with_control("%", true, None, DEFAULT_COLLATION, &control)
            .unwrap();
    let retained = memory.used();
    let error = pattern
        .try_is_match_with_control(&"É".repeat(1024), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), retained);
    let failed = CompiledLikePattern::with_escape_with_control(
        &"a".repeat(1024),
        false,
        None,
        DEFAULT_COLLATION,
        &control,
    );
    assert!(failed.is_err());
    assert_eq!(failed.err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), retained);
    drop(pattern);
    assert_eq!(memory.used(), 0);
}

/// Expectations are `PostgreSQL` 18.4 results under each pinned collation.
#[test]
fn ilike_lowercases_input_and_pattern_through_the_input_collation() {
    let ilike = |input: &str, pattern: &str, escape: Option<&str>, collation: u32| {
        CompiledLikePattern::with_escape(pattern, true, escape, collation)
            .unwrap()
            .try_is_match(input)
            .unwrap()
    };
    // C-type collations fold ASCII letters only; the Unicode providers fold every letter.
    assert!(ilike("abc", "ABC", None, 950));
    assert!(!ilike("\u{c9}", "\u{e9}", None, 950));
    for collation in [811, 963, 6411, 100] {
        assert!(ilike("\u{c9}", "\u{e9}", None, collation), "{collation}");
    }
    // Full mappings lowercase U+0130 to two characters; simple and ASCII mappings keep one.
    for (collation, one, two) in [
        (950, true, false),
        (811, true, false),
        (6411, false, true),
        (963, false, true),
    ] {
        assert_eq!(ilike("\u{130}", "_", None, collation), one, "{collation}");
        assert_eq!(ilike("\u{130}", "__", None, collation), two, "{collation}");
    }
    // The builtin provider's final-sigma test passes a sigma that only case-ignorable characters precede; ICU's does not.
    assert!(ilike("'\u{3a3}", "'\u{3c2}", None, 6411));
    assert!(!ilike("'\u{3a3}", "'\u{3c2}", None, 963));
    // The pattern is rewritten to the backslash escape before it is lowercased, so an uppercase escape still escapes.
    assert!(ilike("a_x", "A#_%", Some("#"), 6411));
    assert!(!ilike("ab", "A#_%", Some("#"), 6411));
    assert!(ilike("_", "A_", Some("A"), 6411));
    assert!(!ilike("a", "A_", Some("A"), 6411));
    // LIKE itself compares characters exactly under every collation.
    for collation in [950, 811, 963, 6411] {
        assert!(
            !CompiledLikePattern::with_escape("ABC", false, None, collation)
                .unwrap()
                .try_is_match("abc")
                .unwrap()
        );
    }
}

#[test]
fn text_patterns_require_a_determinate_collation() {
    for (case_insensitive, operator) in [(false, "LIKE"), (true, "ILIKE")] {
        let Err(crate::SQLError::Diagnostic {
            sqlstate,
            message,
            hint,
            ..
        }) = CompiledLikePattern::with_escape("a", case_insensitive, None, 0)
        else {
            panic!("{operator} needs a collation");
        };
        assert_eq!(sqlstate, "42P22");
        assert_eq!(
            message,
            format!("could not determine which collation to use for {operator}")
        );
        assert_eq!(
            hint.as_deref(),
            Some("Use the COLLATE clause to set the collation explicitly.")
        );
    }
    // Bytea patterns have no collation.
    let pattern = CompiledLikePattern::from_value_with_escape_with_control(
        &Value::Bytes(b"a%".to_vec()),
        false,
        None,
        0,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert!(pattern
        .try_matches_value(&Value::Bytes(b"abc".to_vec()))
        .unwrap());
}
