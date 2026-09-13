//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative match-range searches over prepared regular expressions.

use std::ops::Range;

use regex_automata::{
    dfa::{dense, sparse, Automaton, StartKind},
    nfa::thompson,
    Anchored, Input, MatchKind,
};

use crate::{AnalysisError, AnalysisResult};

const POLL_INTERVAL: usize = 1024;
// Bound each DFA and its determinization workspace; larger patterns use the reserved NFA search.
const DFA_SIZE_LIMIT_BYTES: usize = 1 << 20;

#[derive(Debug)]
pub(super) struct RangeDFA {
    forward: sparse::DFA<Vec<u8>>,
    reverse: sparse::DFA<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) enum SearchError {
    Automaton,
    Poll(AnalysisError),
}

impl RangeDFA {
    /// Compile a match-range search automaton from the same syntax accepted by `regex`.
    ///
    /// Capturing groups are ignored while finding the overall range. Captures, Unicode word boundaries, and size-limited DFA constructions use the prepared NFA traversal.
    pub(crate) fn compile(pattern: &str) -> Option<Self> {
        let forward = dense::Builder::new()
            .thompson(thompson::Config::new().nfa_size_limit(Some(super::NFA_SIZE_LIMIT)))
            .configure(cooperative_dfa_config())
            .build(pattern)
            .ok()?
            .to_sparse()
            .ok()?;
        let reverse = dense::Builder::new()
            .thompson(
                thompson::Config::new()
                    .reverse(true)
                    .nfa_size_limit(Some(super::NFA_SIZE_LIMIT)),
            )
            .configure(
                cooperative_dfa_config()
                    .start_kind(StartKind::Anchored)
                    .match_kind(MatchKind::All),
            )
            .build(pattern)
            .ok()?
            .to_sparse()
            .ok()?;
        Some(Self { forward, reverse })
    }

    pub(crate) fn find_at(
        &self,
        text: &str,
        start: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> Result<Option<Range<usize>>, SearchError> {
        if start > text.len() {
            return Ok(None);
        }
        let mut start = Self::next_boundary(text, start, poll)?;
        loop {
            poll().map_err(SearchError::Poll)?;
            let Some(range) = self.search_range(text, start, poll)? else {
                return Ok(None);
            };
            if text.is_char_boundary(range.end) {
                return Ok(Some(range));
            }
            // UTF-8 NFAs guarantee valid nonempty matches. Empty assertions can still match inside a scalar and must advance to its next boundary.
            debug_assert!(range.is_empty());
            start = Self::next_boundary(text, range.end + 1, poll)?;
        }
    }

    fn search_range(
        &self,
        text: &str,
        start: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> Result<Option<Range<usize>>, SearchError> {
        let bytes = text.as_bytes();
        let input = Input::new(bytes).span(start..bytes.len());
        let mut state = self
            .forward
            .start_state_forward(&input)
            .map_err(|_| SearchError::Automaton)?;
        let mut end = None;
        for (index, &byte) in bytes.iter().enumerate().skip(start) {
            if (index - start).is_multiple_of(POLL_INTERVAL) {
                poll().map_err(SearchError::Poll)?;
            }
            state = self.forward.next_state(state, byte);
            if self.forward.is_quit_state(state) {
                return Err(SearchError::Automaton);
            }
            if self.forward.is_match_state(state) {
                // DFA match states are delayed by one byte so that end-of-input assertions work.
                end = Some(index);
            }
            if self.forward.is_dead_state(state) {
                break;
            }
        }
        state = self.forward.next_eoi_state(state);
        if self.forward.is_quit_state(state) {
            return Err(SearchError::Automaton);
        }
        if self.forward.is_match_state(state) {
            end = Some(bytes.len());
        }
        let Some(end) = end else {
            return Ok(None);
        };

        let input = Input::new(bytes).span(start..end).anchored(Anchored::Yes);
        let mut state = self
            .reverse
            .start_state_reverse(&input)
            .map_err(|_| SearchError::Automaton)?;
        let mut begin = None;
        for index in (start..end).rev() {
            if (end - 1 - index).is_multiple_of(POLL_INTERVAL) {
                poll().map_err(SearchError::Poll)?;
            }
            state = self.reverse.next_state(state, bytes[index]);
            if self.reverse.is_quit_state(state) {
                return Err(SearchError::Automaton);
            }
            if self.reverse.is_match_state(state) {
                begin = index.checked_add(1);
            }
            if self.reverse.is_dead_state(state) {
                break;
            }
        }
        if start > 0 {
            state = self.reverse.next_state(state, bytes[start - 1]);
            if self.reverse.is_quit_state(state) {
                return Err(SearchError::Automaton);
            }
            if self.reverse.is_match_state(state) {
                begin = Some(start);
            }
        } else {
            state = self.reverse.next_eoi_state(state);
            if self.reverse.is_quit_state(state) {
                return Err(SearchError::Automaton);
            }
            if self.reverse.is_match_state(state) {
                begin = Some(start);
            }
        }
        let Some(begin) = begin else {
            return Err(SearchError::Automaton);
        };
        Ok(Some(begin..end))
    }

    fn next_boundary(
        text: &str,
        mut start: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> Result<usize, SearchError> {
        while start < text.len() && !text.is_char_boundary(start) {
            poll().map_err(SearchError::Poll)?;
            start += 1;
        }
        Ok(start)
    }
}

fn cooperative_dfa_config() -> dense::Config {
    dense::Config::new()
        .dfa_size_limit(Some(DFA_SIZE_LIMIT_BYTES))
        .determinize_size_limit(Some(DFA_SIZE_LIMIT_BYTES))
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::{RangeDFA as CooperativeRegex, SearchError};
    use crate::AnalysisError;

    #[test]
    fn cooperative_ranges_match_the_validated_regex() {
        for pattern in [
            r"foo|foobar",
            r"a+",
            r"a*",
            r"(?:ab)+",
            r"(?m)^foo$",
            r"(?m)(b|^ab)",
            r"\w+",
            r"\p{Greek}+",
            r"[^>]+",
            r"\d{2,4}",
            r"(?s).+?",
            r"a?b",
            r"(?i)rust",
            r"(?-u:\b)a+(?-u:\b)",
            r"(?-u:\B)(a*)",
            r"\Afoo\z",
            r"$",
            r"^",
            "",
        ] {
            let expression = Regex::new(pattern).unwrap();
            let cooperative = CooperativeRegex::compile(pattern).unwrap();
            for text in [
                "", "aa", "foobar", "xfoo\n", "\n\rab", "é", "🙂", "aé", "αβfoo", "a1 b22",
            ] {
                for start in 0..=text.len() {
                    if !text.is_char_boundary(start) {
                        continue;
                    }
                    let expected = expression
                        .find_at(text, start)
                        .map(|matched| matched.range());
                    let actual = cooperative.find_at(text, start, &mut || Ok(())).unwrap();
                    assert_eq!(
                        actual, expected,
                        "pattern={pattern:?}, text={text:?}, start={start}"
                    );
                }
            }
        }
    }

    #[test]
    fn cooperative_scan_polls_through_a_long_unmatched_input() {
        let cooperative = CooperativeRegex::compile(r"needle").unwrap();
        let text = "x".repeat(128 * 1024);
        let mut polls = 0;
        cooperative
            .find_at(&text, 0, &mut || {
                polls += 1;
                Ok(())
            })
            .unwrap();
        assert!(polls > text.len() / 2048);
    }

    #[test]
    fn cooperative_scan_cancellation_is_propagated() {
        let cooperative = CooperativeRegex::compile(r"needle").unwrap();
        let text = "x".repeat(128 * 1024);
        let mut polls = 0;
        let result = cooperative.find_at(&text, 0, &mut || {
            polls += 1;
            if polls == 8 {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(SearchError::Poll(AnalysisError::Cancelled))
        ));
        assert_eq!(polls, 8);
    }

    #[test]
    fn unicode_word_boundaries_require_nfa_execution() {
        assert!(CooperativeRegex::compile(r"\bword\b").is_none());
        assert!(CooperativeRegex::compile(r"(?-u)\bword\b").is_some());
        assert!(CooperativeRegex::compile(r"\\bword\\b").is_some());
    }

    #[test]
    fn capturing_groups_keep_the_overall_range_cooperative() {
        let pattern = r"(foo|bar)+(?<tail>baz)?";
        let expression = Regex::new(pattern).unwrap();
        let cooperative = CooperativeRegex::compile(pattern).unwrap();
        for text in ["foo", "foobar", "foobar-baz", "xbarbaz"] {
            for start in 0..=text.len() {
                if !text.is_char_boundary(start) {
                    continue;
                }
                let expected = expression
                    .find_at(text, start)
                    .map(|matched| matched.range());
                let actual = cooperative.find_at(text, start, &mut || Ok(())).unwrap();
                assert_eq!(actual, expected, "text={text:?}, start={start}");
            }
        }
    }

    #[test]
    fn anchored_alternatives_keep_their_original_context_after_restart() {
        let pattern = r"(^ab|b|x)";
        let expression = Regex::new(pattern).unwrap();
        let cooperative = CooperativeRegex::compile(pattern).unwrap();
        let text = "xab";
        let start = 1;
        let expected = expression
            .find_at(text, start)
            .map(|matched| matched.range());
        let actual = cooperative.find_at(text, start, &mut || Ok(())).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn pathological_dfa_construction_stays_within_compilation_limits() {
        assert!(CooperativeRegex::compile(r"([ab]*a[ab]{16})").is_none());
    }
}
