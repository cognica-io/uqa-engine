//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One call's use of the `PostgreSQL` engine: compilation and execution with `PostgreSQL`'s diagnostics, and the code-point form of text the engine searches.

use crate::error::{Result, SQLError};
use std::cell::Cell;
use std::ops::Range;
use uqa_core::memory::{MemoryError, Produced, ProductionControl, ProductionString, ProductionVec};
use uqa_pg_regex::{CompileFlags, MatchSpan, Regex, RegexCollation, RegexError, REG_ESPACE};

mod allowance;
pub(super) use allowance::Allowance;

/// The engine operations of one SQL function call. The call keeps the first refusal of its allowance, so an exhausted allowance is reported as the allowance's own error rather than as the engine's `REG_ESPACE`.
pub(super) struct RegexCall<'c> {
    control: ProductionControl<'c>,
    refusal: Cell<Option<MemoryError>>,
}

impl<'c> RegexCall<'c> {
    pub(super) fn new(control: &ProductionControl<'c>) -> Self {
        Self {
            control: *control,
            refusal: Cell::new(None),
        }
    }

    /// Hooks that charge the engine's allocations to this call's allowance and answer its cancellation and stack checks.
    pub(super) fn allowance(&self) -> Allowance<'_, 'c> {
        Allowance::new(self)
    }

    /// Compile `pattern` as `RE_compile_and_cache` does: the collation is applied first, as `pg_regcomp` calls `pg_set_regex_collation` before parsing, and a failure is `invalid regular expression: <explanation>`.
    pub(super) fn compile<'r>(
        &self,
        pattern: &str,
        flags: CompileFlags,
        collation: u32,
        allowance: &'r mut Allowance<'_, 'c>,
    ) -> Result<Regex<'r>> {
        self.check()?;
        let collation = RegexCollation::from_oid(collation)
            .map_err(|error| self.failure(error, "invalid regular expression"))?;
        Regex::compile(pattern, flags, collation, allowance)
            .map_err(|error| self.failure(error, "invalid regular expression"))
    }

    /// Search `input` from code point `start` as `RE_wchar_execute` does, filling one span per slot of `spans`; a failure is `regular expression failed: <explanation>`.
    pub(super) fn execute(
        &self,
        regex: &mut Regex<'_>,
        input: &[u32],
        start: usize,
        spans: &mut [MatchSpan],
    ) -> Result<bool> {
        regex
            .exec(input, start, spans)
            .map_err(|error| self.failure(error, "regular expression failed"))
    }

    /// `CHECK_FOR_INTERRUPTS` between engine operations.
    pub(super) fn check(&self) -> Result<()> {
        self.control.check()?;
        Ok(())
    }

    fn refuse(&self, error: MemoryError) {
        let first = self.refusal.take().unwrap_or(error);
        self.refusal.set(Some(first));
    }

    fn cancel_requested(&self) -> bool {
        self.control.check_cancellation().is_err()
    }

    fn failure(&self, error: RegexError, context: &str) -> SQLError {
        // A refused reservation fails inside the engine as `REG_ESPACE` and in the wrapper's own reservations as `OutOfMemory`; either reports the allowance's error.
        if matches!(
            error,
            RegexError::Code(REG_ESPACE) | RegexError::OutOfMemory
        ) {
            if let Some(refusal) = self.refusal.take() {
                return refusal.into();
            }
        }
        match error {
            RegexError::Code(_) => engine_error(error, context),
            RegexError::Cancelled => match self.control.check_cancellation() {
                Err(cancelled) => cancelled.into(),
                Ok(()) => SQLError::Internal(
                    "the regular expression engine stopped for a cancellation the statement does not report".into(),
                ),
            },
            other => SQLError::Diagnostic {
                sqlstate: other.sqlstate().unwrap_or("XX000").into(),
                message: other.message().to_string(),
                detail: None,
                hint: other.hint().map(Into::into),
            },
        }
    }
}

fn engine_error(error: RegexError, context: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "2201B".into(),
        message: format!("{context}: {}", error.message()),
    }
}

/// The code points of `text`, the form `pg_mb2wchar_with_len` gives the engine.
pub(super) fn code_points(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u32>>> {
    let mut points = ProductionVec::new(*control);
    points.reserve(text.chars().count())?;
    for character in text.chars() {
        points.push_copy(u32::from(character))?;
    }
    points.finish().map_err(Into::into)
}

/// Append the text of code points `range` of `points`, as `pg_wchar2mb_with_len` converts code points back.
pub(super) fn push_text(
    output: &mut ProductionString<'_>,
    points: &[u32],
    range: Range<usize>,
) -> Result<()> {
    for &point in span_points(points, range)? {
        let character = char::from_u32(point).ok_or_else(|| {
            SQLError::Internal(format!(
                "regular expression input held invalid code point {point}"
            ))
        })?;
        output.push(character)?;
    }
    Ok(())
}

/// The text of code points `range` of `points`.
pub(super) fn text(
    points: &[u32],
    range: Range<usize>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.reserve(
        span_points(points, range.clone())?
            .iter()
            .map(|point| char::from_u32(*point).map_or(0, char::len_utf8))
            .sum(),
    )?;
    push_text(&mut output, points, range)?;
    output.finish().map_err(Into::into)
}

fn span_points(points: &[u32], range: Range<usize>) -> Result<&[u32]> {
    points.get(range).ok_or_else(|| {
        SQLError::Internal("a regular expression span lies outside its input".into())
    })
}

/// The code-point range of a span the engine set, or an internal error for an unset whole-match span.
pub(super) fn whole_match(span: MatchSpan) -> Result<Range<usize>> {
    span.range().ok_or_else(|| {
        SQLError::Internal("the regular expression engine reported a match without its span".into())
    })
}

#[cfg(test)]
mod tests;
