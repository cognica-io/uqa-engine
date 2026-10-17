//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::flags::{REG_ADVANCED, REG_ICASE};

#[derive(Default)]
struct Counting {
    reserved: usize,
    released: usize,
}

impl RegexResources for Counting {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.reserved += bytes;
        true
    }

    fn release(&mut self, bytes: usize) {
        self.released += bytes;
    }

    fn cancel_requested(&mut self) -> bool {
        false
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        depth > 1 << 20
    }
}

fn code_points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

#[test]
fn match_span_mirrors_regmatch_t() {
    // SAFETY: the function only reports a size.
    let size = unsafe { ffi::uqa_pg_regex_match_size() };
    assert_eq!(size, size_of::<MatchSpan>());
}

#[test]
fn leftmost_longest_alternation_and_balanced_accounting() {
    let mut resources = Counting::default();
    {
        let mut regex =
            Regex::compile("a|ab", REG_ADVANCED, RegexCollation::C, &mut resources).unwrap();
        assert_eq!(regex.subexpression_count(), 0);
        assert!(regex.reserved_bytes() > 0);
        let mut spans = [MatchSpan::UNSET; 1];
        assert!(regex.exec(&code_points("ab"), 0, &mut spans).unwrap());
        assert_eq!(spans[0].range(), Some(0..2));
        assert!(!regex.exec(&code_points("xyz"), 0, &mut spans).unwrap());
        assert_eq!(spans[0], MatchSpan::UNSET);
    }
    assert!(resources.reserved > 0);
    assert_eq!(resources.reserved, resources.released);
}

#[test]
fn invalid_pattern_reports_pg_regerror_text_and_releases_everything() {
    let mut resources = Counting::default();
    let error = Regex::compile("(", REG_ADVANCED, RegexCollation::C, &mut resources)
        .err()
        .unwrap();
    assert_eq!(error.code(), Some(crate::flags::REG_EPAREN));
    assert_eq!(error.message(), "parentheses () not balanced");
    assert_eq!(resources.reserved, resources.released);
}

#[test]
fn case_insensitive_matching_follows_the_collation() {
    let mut resources = Counting::default();
    let input = code_points("\u{c9}");
    for (collation, expected) in [
        (RegexCollation::C, false),
        (RegexCollation::BuiltinCUtf8, true),
        (RegexCollation::BuiltinUnicodeFast, true),
        (RegexCollation::Icu, true),
    ] {
        let mut regex = Regex::compile(
            "\u{e9}",
            REG_ADVANCED | REG_ICASE,
            collation,
            &mut resources,
        )
        .unwrap();
        assert_eq!(
            regex.exec(&input, 0, &mut []).unwrap(),
            expected,
            "{collation:?}"
        );
    }
    assert_eq!(resources.reserved, resources.released);
}
