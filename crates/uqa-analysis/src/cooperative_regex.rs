//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative, capture-free searches over prepared regular expressions.

use std::ops::Range;

use regex_automata::{
    dfa::{dense, sparse, Automaton, StartKind},
    nfa::thompson,
    Anchored, Input, MatchKind,
};

use crate::{AnalysisError, AnalysisResult};

const POLL_INTERVAL: usize = 1024;

#[derive(Debug)]
pub(crate) struct CooperativeRegex {
    forward: sparse::DFA<Vec<u8>>,
    reverse: sparse::DFA<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) enum SearchError {
    Automaton,
    Poll(AnalysisError),
}

impl CooperativeRegex {
    /// Compile a capture-free search automaton from the same syntax accepted by `regex`.
    ///
    /// Unicode word-boundary expressions are intentionally left to the `regex` fallback: the
    /// pinned DFA builder cannot represent their full Unicode look-around semantics. A failed
    /// DFA construction likewise keeps the validated library expression as the fallback.
    pub(crate) fn compile(pattern: &str) -> Option<Self> {
        if pattern.contains(r"\b") || pattern.contains(r"\B") {
            return None;
        }
        let forward = dense::Builder::new()
            .build(pattern)
            .ok()?
            .to_sparse()
            .ok()?;
        let reverse = dense::Builder::new()
            .thompson(thompson::Config::new().reverse(true))
            .configure(
                dense::Config::new()
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
        let start = Self::next_boundary(text, start, poll)?;
        let bytes = text.as_bytes();
        let input = Input::new(bytes).range(start..);
        let mut state = self
            .forward
            .start_state_forward(&input)
            .map_err(|_| SearchError::Automaton)?;
        let mut end = None;
        for (index, &byte) in bytes.iter().enumerate().skip(start) {
            if (index - start) % POLL_INTERVAL == 0 {
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

        let input = Input::new(bytes).range(start..end).anchored(Anchored::Yes);
        let mut state = self
            .reverse
            .start_state_reverse(&input)
            .map_err(|_| SearchError::Automaton)?;
        let mut begin = None;
        for index in (start..end).rev() {
            if (end - 1 - index) % POLL_INTERVAL == 0 {
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
        state = self.reverse.next_eoi_state(state);
        if self.reverse.is_quit_state(state) {
            return Err(SearchError::Automaton);
        }
        if self.reverse.is_match_state(state) {
            begin = Some(start);
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

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::{CooperativeRegex, SearchError};
    use crate::AnalysisError;

    #[test]
    fn cooperative_ranges_match_the_validated_regex() {
        for pattern in [
            r"foo|foobar",
            r"a+",
            r"a*",
            r"(?:ab)+",
            r"(?m)^foo$",
            r"\w+",
            r"\p{Greek}+",
            r"[^>]+",
            r"\d{2,4}",
            r"(?s).+?",
            r"a?b",
            r"(?i)rust",
            r"\Afoo\z",
            r"$",
            r"^",
            "",
        ] {
            let expression = Regex::new(pattern).unwrap();
            let cooperative = CooperativeRegex::compile(pattern).unwrap();
            for text in [
                "", "aa", "foobar", "xfoo\n", "é", "🙂", "aé", "αβfoo", "a1 b22",
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
    fn word_boundaries_use_the_library_fallback() {
        assert!(CooperativeRegex::compile(r"\bword\b").is_none());
        assert!(CooperativeRegex::compile(r"(?-u)\bword\b").is_none());
    }
}
