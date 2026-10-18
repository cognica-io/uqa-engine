//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::boundaries;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn segments(text: &str) -> Vec<&str> {
    let offsets = boundaries(text, &ProductionControl::uncontrolled())
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    offsets
        .windows(2)
        .map(|pair| &text[pair[0]..pair[1]])
        .collect()
}

#[test]
fn root_rules_tailor_uax29_as_icu_does() {
    // Letters join across apostrophes and periods, but ICU removes colons from MidLetter.
    assert_eq!(segments("o'neil a.b"), ["o'neil", " ", "a.b"]);
    assert_eq!(segments("a:b"), ["a", ":", "b"]);
    // Numbers join letters and their own separators; ExtendNumLet joins both.
    assert_eq!(segments("1,000.5x y_z"), ["1,000.5x", " ", "y_z"]);
    // Hangul syllables and kana/kanji chain only with themselves, while complex-context letters join ALetter.
    assert_eq!(segments("\u{d55c}\u{ad6d}abc"), ["\u{d55c}\u{ad6d}", "abc"]);
    assert_eq!(segments("\u{65e5}\u{3042}abc"), ["\u{65e5}\u{3042}", "abc"]);
    assert_eq!(segments("\u{e44}\u{e17}abc"), ["\u{e44}\u{e17}abc"]);
    // Extend, Format and ZWJ attach to the preceding character except at the start of text and after line ends.
    assert_eq!(segments("\u{301}a\u{301}b"), ["\u{301}", "a\u{301}b"]);
    assert_eq!(segments("a\r\n\u{301}b"), ["a", "\r\n", "\u{301}", "b"]);
    assert_eq!(
        segments("\u{1f469}\u{200d}\u{1f469}x"),
        ["\u{1f469}\u{200d}\u{1f469}", "x"]
    );
    // Regional indicators pair from the start of their run.
    assert_eq!(
        segments("\u{1f1fa}\u{1f1f8}\u{1f1f0}"),
        ["\u{1f1fa}\u{1f1f8}", "\u{1f1f0}"]
    );
    assert_eq!(segments("a  b"), ["a", "  ", "b"]);
    assert!(segments("").is_empty());
}

#[test]
fn boundaries_admit_their_scratch_and_observe_cancellation() {
    let memory = MemoryBudget::new(64 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let offsets = boundaries("alpha beta", &control).unwrap();
    assert_eq!(*offsets, [0, 5, 6, 10]);
    assert_eq!(memory.used(), offsets.capacity() * size_of::<usize>());
    drop(offsets);
    assert_eq!(memory.used(), 0);
    original.cancel();
    assert!(boundaries("alpha", &control).is_err());
    assert_eq!(memory.used(), 0);
}
