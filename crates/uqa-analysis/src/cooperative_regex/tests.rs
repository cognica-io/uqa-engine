//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use regex::Regex;
use uqa_core::memory::{MemoryBudget, MemoryError};

use super::CooperativeRegex;
use crate::AnalysisError;

#[test]
fn cooperative_ranges_and_captures_match_regex_with_and_without_dfa_ranges() {
    for pattern in [
        r"(foo|foobar)",
        r"(a*)(a*?)",
        r"((a?)*)",
        r"((a*)*)",
        r"((a?){2})*",
        r"((a)|(ab))*",
        r"(?:a|)(b?)",
        r"(a|ab)(b|)",
        r"(^a|a$)|(a)",
        r"(?m)(a\n|$)",
        r"(?s:.*)(a|ab)",
        r"(a+?){2}",
        r"a(?:b|a?)*b",
        r"(?U)(a*)(a+)",
        r"(?-u:[\x00-\x7F]+)",
        r"(a|ab)*",
        r"(^ab|b|x)",
        r"(?m)(b|^ab)",
        r"(?m)(^|$)",
        r"(?mR)(^.*$)",
        r"(?<x>韓|a)(?<y>🙂)?",
        r"(?P<x>a)?",
        r"(?P<한>韓|a)",
        r"\b(\w+)\b",
        r"\B(\w+)\B",
        r"(?-u:\b)(a+)",
        r"(?-u:\B)(a*)",
        r"\b{start}(\w+)\b{end}",
        r"\b{start-half}(a*)\b{end-half}",
        r"\p{Greek}+",
        r"(?s)(.+?)",
        r"(?i)(rust)",
        r"\A(foo)\z",
        r"",
        r"$",
    ] {
        let oracle = Regex::new(pattern).unwrap();
        let mut expression = CooperativeRegex::compile(pattern).unwrap();
        for use_dfa in [true, false] {
            if !use_dfa {
                expression.dfa = None;
            }
            for capture_groups in [true, false] {
                let budget = MemoryBudget::new(16 << 20);
                let mut search = expression
                    .searcher(&budget, capture_groups, &mut || Ok(()))
                    .unwrap();
                let mut captures = oracle.capture_locations();
                for text in [
                    "",
                    "aa",
                    "aba",
                    "xab",
                    "foobar",
                    "xfoo\n",
                    "\n\rab",
                    "é",
                    "🙂",
                    "aé",
                    "αβfoo",
                    "韓🙂a",
                    "🙂\n韓",
                    "\r\na\r\n",
                    "RUST",
                ] {
                    for start in 0..=text.len() + 1 {
                        let expected = oracle
                            .captures_read_at(&mut captures, text, start)
                            .map(|m| m.range());
                        let actual = search.find_at(text, start, &mut || Ok(())).unwrap();
                        assert_eq!(
                            actual, expected,
                            "{pattern:?}, {text:?}, {start}, dfa={use_dfa}"
                        );
                        if actual.is_some() && capture_groups {
                            for group in 0..oracle.captures_len() {
                                assert_eq!(
                                    search.captures().unwrap().get(group),
                                    captures.get(group),
                                    "{pattern:?}, {text:?}, {start}, group={group}, dfa={use_dfa}"
                                );
                            }
                        } else {
                            assert!(search.captures().is_none());
                        }
                    }
                }
                drop(search);
                assert_eq!(budget.used(), 0);
            }
        }
    }
}

#[test]
fn determinization_limits_keep_captures_available_through_reserved_nfa_execution() {
    let expression = CooperativeRegex::compile(r"([ab]*a[ab]{16})").unwrap();
    assert!(expression.dfa.is_none());
    let budget = MemoryBudget::new(1 << 20);
    let mut search = expression.searcher(&budget, true, &mut || Ok(())).unwrap();
    let text = format!("{} a{}", "x".repeat(8192), "b".repeat(16));
    assert_eq!(
        search.find_at(&text, 0, &mut || Ok(())).unwrap(),
        Some(8193..8210)
    );
    assert_eq!(search.captures().unwrap().get(1), Some((8193, 8210)));
    assert_eq!(search.find_at(&text, 8194, &mut || Ok(())).unwrap(), None);
    assert!(search.captures().is_none());
    drop(search);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unicode_word_boundary_searches_poll_and_preserve_capture_ranges() {
    let expression = CooperativeRegex::compile(r"\b(needle)\b").unwrap();
    assert!(expression.dfa.is_none());
    let text = format!("{} needle", "x".repeat(128 * 1024));
    let budget = MemoryBudget::new(1 << 20);
    let mut search = expression.searcher(&budget, true, &mut || Ok(())).unwrap();
    let mut polls = 0;
    let found = search
        .find_at(&text, 0, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(found, Some(text.len() - 6..text.len()));
    assert_eq!(
        search.captures().unwrap().get(1),
        Some((text.len() - 6, text.len()))
    );
    assert!(polls > text.len() / 1024);
    drop(search);
    assert_eq!(budget.used(), 0);
}

#[test]
fn capture_resolution_can_cancel_after_the_cooperative_range_scan() {
    let expression = CooperativeRegex::compile(r"(x+)").unwrap();
    let text = "x".repeat(128 * 1024);
    let budget = MemoryBudget::new(1 << 20);
    let mut range = expression.searcher(&budget, false, &mut || Ok(())).unwrap();
    let mut range_polls = 0;
    range
        .find_at(&text, 0, &mut || {
            range_polls += 1;
            Ok(())
        })
        .unwrap();
    let mut search = expression.searcher(&budget, true, &mut || Ok(())).unwrap();
    // Prepare the reusable workspace so that the cancellation lands in capture traversal.
    search.find_at("x", 0, &mut || Ok(())).unwrap();
    let mut polls = 0;
    let result = search.find_at(&text, 0, &mut || {
        polls += 1;
        if polls == range_polls + 8 {
            Err(AnalysisError::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(AnalysisError::Cancelled)));
    assert_eq!(polls, range_polls + 8);
    assert!(search.captures().is_none());
    assert_eq!(search.find_at("xx", 0, &mut || Ok(())).unwrap(), Some(0..2));
    assert_eq!(search.captures().unwrap().get(1), Some((0, 2)));
    drop(search);
    drop(range);
    assert_eq!(budget.used(), 0);
}

#[test]
fn nfa_workspace_limits_preserve_independent_allocations() {
    let mut expression = CooperativeRegex::compile(r"(a+?)(a*)").unwrap();
    expression.dfa = None;
    let baseline = MemoryBudget::new(1 << 20);
    {
        let mut search = expression
            .searcher(&baseline, true, &mut || Ok(()))
            .unwrap();
        assert_eq!(
            search.find_at("aaaa", 0, &mut || Ok(())).unwrap(),
            Some(0..4)
        );
    }
    let required = baseline.peak();
    for allowance in 0..=required {
        let budget = MemoryBudget::new(17 + allowance);
        let independent = budget.reserve(17).unwrap();
        let result = (|| {
            let mut search = expression.searcher(&budget, true, &mut || Ok(()))?;
            search.find_at("aaaa", 0, &mut || Ok(()))
        })();
        if allowance < required {
            assert!(
                matches!(
                    result,
                    Err(AnalysisError::Memory(MemoryError::Limit { .. }))
                ),
                "allowance={allowance}, required={required}"
            );
        } else {
            assert_eq!(result.unwrap(), Some(0..4));
        }
        assert_eq!(budget.used(), 17);
        drop(independent);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn every_nfa_cancellation_boundary_releases_workspace() {
    let expression = CooperativeRegex::compile(r"\b(?<word>a+)\b").unwrap();
    let text = "a".repeat(2048);
    let budget = MemoryBudget::new(1 << 20);
    let independent = budget.reserve(17).unwrap();
    let mut checks = 0;
    {
        let mut poll = || {
            checks += 1;
            Ok(())
        };
        let mut search = expression.searcher(&budget, true, &mut poll).unwrap();
        assert_eq!(
            search.find_at(&text, 0, &mut poll).unwrap(),
            Some(0..text.len())
        );
    }
    for stop in 1..=checks {
        let mut calls = 0;
        let mut poll = || {
            calls += 1;
            if calls == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        };
        let result = (|| {
            let mut search = expression.searcher(&budget, true, &mut poll)?;
            search.find_at(&text, 0, &mut poll)
        })();
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "stop={stop}"
        );
        assert_eq!(budget.used(), 17, "stop={stop}");
    }
    drop(independent);
    assert_eq!(budget.used(), 0);
}
