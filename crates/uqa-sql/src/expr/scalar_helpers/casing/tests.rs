//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{casefold, lowercase, uppercase};
use icu_casemap::CaseMapper;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn streaming_case_mapping_matches_pinned_unicode_scalar_mappings() {
    // One deterministic corpus exercises every scalar with non-cased separators, so a context-sensitive sigma cannot disguise a scalar mapping difference.
    let mut input = String::new();
    for character in (0..=0x10_ffff).filter_map(char::from_u32) {
        input.push(character);
        input.push('\0');
    }
    let memory = MemoryBudget::new(64 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let lower = lowercase(&input, &control).unwrap();
    assert_eq!(lower.as_str(), input.to_lowercase());
    assert_eq!(memory.used(), lower.capacity());
    drop(lower);
    let upper = uppercase(&input, &control).unwrap();
    assert_eq!(upper.as_str(), input.to_uppercase());
    assert_eq!(memory.used(), upper.capacity());
    drop(upper);
    let fold = casefold(&input, &control).unwrap();
    assert_eq!(
        fold.as_str(),
        CaseMapper::new().fold_string(&input).as_ref()
    );
    assert_eq!(memory.used(), fold.capacity());
    drop(fold);
    assert_eq!(memory.used(), 0);
}

#[test]
fn streaming_lowercase_preserves_final_sigma_and_case_ignorable_context() {
    let memory = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for input in [
        "Σ",
        "ΟΣ",
        "ΟΣΑ",
        "Ο\u{301}Σ",
        "ΟΣ\u{301}",
        "ΟΣ\u{301}Α",
        "Ο'Σ",
        "ΟΣ'Α",
        "Ο\u{200d}Σ\u{200d}",
        "İ I ΣΣ",
    ] {
        let result = lowercase(input, &control).unwrap();
        assert_eq!(result.as_str(), input.to_lowercase(), "{input:?}");
        assert_eq!(memory.used(), result.capacity());
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}
