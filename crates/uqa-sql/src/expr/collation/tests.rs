//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn controlled_unicode_preserves_postgresql_normalization_mode() {
    let reference = icu_collator::Collator::try_new(
        icu_collator::CollatorPreferences::default(),
        icu_collator::options::CollatorOptions::default(),
    )
    .unwrap();
    let left = "a\u{301}\u{323}";
    let right = "å";
    // PostgreSQL's built-in follows und-u-kk-false, while ICU4X's complete-string API normalizes this sequence.
    assert_eq!(reference.compare(left, right), Ordering::Greater);
    assert_eq!(
        compare_unicode_with_control(left, right, &ProductionControl::uncontrolled()).unwrap(),
        Ordering::Less
    );
}

#[test]
fn controlled_unicode_matches_postgresql_root_order() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let values = fixture["values"].as_array().unwrap();
    let budget = uqa_core::memory::MemoryBudget::new(1024 * 1024);
    let original = uqa_core::CancellationToken::new();
    let invoking = uqa_core::CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut differences = Vec::new();
    for (i, left) in values.iter().enumerate() {
        for (j, right) in values.iter().enumerate() {
            let left = left.as_str().unwrap();
            let right = right.as_str().unwrap();
            let expected = fixture["orders"][i][j].as_i64().unwrap().cmp(&0);
            let actual = compare_unicode_with_control(left, right, &control).unwrap();
            if actual != expected {
                differences.push(format!("{left:?}, {right:?}: {actual:?} != {expected:?}"));
            }
            assert_eq!(budget.used(), 0);
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences: {:?}",
        differences.len(),
        differences.iter().take(20).collect::<Vec<_>>()
    );
}

#[test]
fn unicode_comparison_admits_scratch_and_releases_every_lease() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let text = format!("a{}", "\u{301}\u{323}".repeat(512));
    let other = format!("a{}", "\u{323}\u{301}".repeat(512));
    let budget = MemoryBudget::new(256 * 1024);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let expected = Ordering::Less;
    assert_eq!(
        compare_unicode_with_control(&text, &other, &control).unwrap(),
        expected
    );
    assert!(budget.peak() > 0);
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        compare_unicode_with_control("B", "a", &control).unwrap(),
        Ordering::Greater
    );
    assert_eq!(
        compare_unicode_with_control(&text, &text, &control).unwrap(),
        Ordering::Equal
    );
    assert_eq!(
        compare_unicode_with_control(&text, &other, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.peak(), 0);
    for original_scope in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_scope {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (left, right) in [("", ""), ("B", "a"), (&text, &other)] {
            assert_eq!(
                compare_unicode_with_control(left, right, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        }
    }
}

#[test]
fn unicode_decomposition_observes_both_cancellation_scopes_during_input() {
    use std::cell::Cell;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    for original_scope in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let budget = MemoryBudget::new(128 * 1024);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let reads = Cell::new(0);
        let input = std::iter::once('a')
            .chain(std::iter::repeat_n('\u{301}', 2048))
            .inspect(|_| {
                reads.set(reads.get() + 1);
                if reads.get() == 64 {
                    if original_scope {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                }
            });
        let diacritics =
            <Baked as DataProvider<CollationDiacriticsV1>>::load(&Baked, DataRequest::default())
                .unwrap()
                .payload;
        let root = Baked::SINGLETON_COLLATION_ROOT_V1;
        let jamo = Baked::SINGLETON_COLLATION_JAMO_V1
            .ce32s
            .as_ule_slice()
            .try_into()
            .unwrap();
        let mut elements = CollationElements::new(
            input,
            control,
            root,
            jamo,
            &diacritics.get().secondaries,
            icu_normalizer::provider::Baked::SINGLETON_NORMALIZER_NFD_DATA_V1,
            icu_normalizer::provider::Baked::SINGLETON_NORMALIZER_NFD_TABLES_V1,
        );
        elements.init().unwrap();
        assert_eq!(
            SQLError::from(elements.next().unwrap_err()).sqlstate(),
            Some("57014")
        );
        assert_eq!(reads.get(), 64);
        drop(elements);
        assert!(budget.peak() > 0);
        assert_eq!(budget.used(), 0);
    }
}
