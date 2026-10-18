//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s `setup_regexp_matches` and `build_regexp_match_result`: every match of one search, found before any result is returned, and the `text[]` value of one match.

use super::{engine, flags::ReFlags, plain, string};
use crate::error::{Result, SQLError};
use std::ops::Range;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};
use uqa_pg_regex::{MatchSpan, REG_NOSUB};

/// `MaxAllocSize / sizeof(int)`: the most match locations `setup_regexp_matches` can hold, the input length it stores after the last match included.
const MAX_LOCATIONS: usize = 0x3fff_ffff / 4;

/// Where and how one search looks for matches, the arguments of `setup_regexp_matches`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Search {
    /// The code point at which the first search starts.
    pub(super) start: usize,
    /// Whether to record each match's parenthesized subexpressions instead of the whole match.
    pub(super) subexpressions: bool,
    /// Whether to skip degenerate matches: empty matches at the start or end of the input or right after the previous match.
    pub(super) ignore_degenerate: bool,
}

/// The matches of one search over an input's code points, as `setup_regexp_matches` records them: `patterns` spans per match.
#[derive(Debug)]
pub(super) struct MatchSet {
    input: Produced<Vec<u32>>,
    patterns: usize,
    spans: Produced<Vec<MatchSpan>>,
}

impl MatchSet {
    /// Search `input` for `pattern` from `search.start`, recording every match when `flags` requests `g` and the first one otherwise. Each search starts where the previous match ended, or one code point later after an empty match, and stops past the end of the input.
    pub(super) fn find(
        input: &str,
        pattern: &str,
        flags: ReFlags,
        collation: u32,
        search: Search,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let points = engine::code_points(input, control)?;
        let call = engine::RegexCall::new(control);
        let mut allowance = call.allowance();
        let mut cflags = flags.cflags;
        if !search.subexpressions {
            cflags |= REG_NOSUB;
        }
        let mut regex = call.compile(pattern, cflags, collation, &mut allowance)?;
        let subexpressions = if search.subexpressions {
            regex.subexpression_count()
        } else {
            0
        };
        let patterns = subexpressions.max(1);
        let mut slots = ProductionVec::new(*control);
        slots.reserve(subexpressions + 1)?;
        for _ in 0..=subexpressions {
            slots.push_copy(MatchSpan::UNSET)?;
        }
        let mut spans = ProductionVec::new(*control);
        let mut start = search.start;
        let mut previous_end = 0;
        while call.execute(&mut regex, &points, start, slots.as_mut_slice())? {
            call.check()?;
            let whole = engine::whole_match(slots[0])?;
            if !search.ignore_degenerate || (whole.start < points.len() && whole.end > previous_end)
            {
                if 2 * (spans.len() + patterns) + 1 > MAX_LOCATIONS {
                    return Err(SQLError::Routine {
                        sqlstate: "54000".into(),
                        message: "too many regular expression matches".into(),
                    });
                }
                if subexpressions == 0 {
                    spans.push_copy(slots[0])?;
                } else {
                    spans.extend_from_slice(&slots[1..])?;
                }
            }
            previous_end = whole.end;
            if !flags.global {
                break;
            }
            start = whole.end + usize::from(whole.is_empty());
            if start > points.len() {
                break;
            }
        }
        drop(regex);
        Ok(Self {
            input: points,
            patterns,
            spans: spans.finish()?,
        })
    }

    /// The number of matches found, `nmatches`.
    pub(super) fn len(&self) -> usize {
        self.spans.len() / self.patterns
    }

    /// The spans recorded per match: the subexpressions, or one for the whole match.
    pub(super) const fn patterns(&self) -> usize {
        self.patterns
    }

    /// The input's length in code points.
    pub(super) fn input_len(&self) -> usize {
        self.input.len()
    }

    /// Span `pattern` (zero-based) of match `index`.
    pub(super) fn span(&self, index: usize, pattern: usize) -> MatchSpan {
        self.spans
            .get(index * self.patterns + pattern)
            .copied()
            .unwrap_or(MatchSpan::UNSET)
    }

    /// The text of `span`, or `None` when the engine left it unset.
    pub(super) fn text(
        &self,
        span: MatchSpan,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>> {
        span.range()
            .map(|range| engine::text(&self.input, range, control))
            .transpose()
    }

    /// The text of code points `range` of the input.
    pub(super) fn slice(
        &self,
        range: Range<usize>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        engine::text(&self.input, range, control)
    }

    /// The `text[]` value of match `index`, `build_regexp_match_result`: one element per recorded span, NULL where a subexpression did not take part in the match.
    pub(super) fn row(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        let mut elements = ProductionVec::new(*control);
        elements.reserve(self.patterns)?;
        for pattern in 0..self.patterns {
            let element = match self.text(self.span(index, pattern), control)? {
                Some(text) => string(text, control)?,
                None => plain(Value::Null, control)?,
            };
            elements.push_produced(element)?;
        }
        let array =
            ArrayValue::try_new_with_control(elements.finish()?, control)?.ok_or_else(|| {
                SQLError::Internal("a regular expression match produced an invalid array".into())
            })?;
        let (array, memory) = array.into_parts();
        control
            .finish(Value::Array(array), memory)
            .map_err(Into::into)
    }
}
