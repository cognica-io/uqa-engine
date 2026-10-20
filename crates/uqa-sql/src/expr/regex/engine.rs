//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One call's use of the `PostgreSQL` engine: compilation and execution with `PostgreSQL`'s diagnostics, and the code-point form of text the engine searches.

use super::cache::{current_regex_cache, Entry, RegexCache};
use crate::error::{Result, SQLError};
use std::cell::Cell;
use std::ops::Range;
use std::sync::Arc;
use uqa_core::memory::{MemoryError, Produced, ProductionControl, ProductionString, ProductionVec};
use uqa_pg_regex::{
    CompileFlags, MatchSpan, Regex, RegexCollation, RegexError, MAX_ALLOC_SIZE, REG_ESPACE,
};

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
    pub(super) fn compile<'r, 'a>(
        &self,
        pattern: &str,
        flags: CompileFlags,
        collation: u32,
        allowance: &'r mut Allowance<'a, 'c>,
    ) -> Result<Invocation<'r, 'a, 'c>> {
        self.check()?;
        // `RE_compile_and_cache` converts the pattern before `pg_regcomp` applies the collation.
        code_point_buffer_bytes(pattern.len())?;
        let collation_oid = collation;
        let collation = RegexCollation::from_oid(collation)
            .map_err(|error| self.failure(error, "invalid regular expression"))?;
        let cache = current_regex_cache();
        if let Some(cache) = &cache {
            if let Some(entry) = cache.take(pattern, flags, collation_oid) {
                return Ok(Invocation::Cached {
                    entry: Some(entry),
                    cache: cache.clone(),
                });
            }
        }
        let generation = cache.as_ref().map(|cache| cache.generation());
        // Compilation scratch belongs to this invocation. Cache capacity must not change valid patterns or PostgreSQL's compilation diagnostics.
        let compiled = Regex::compile_with_resources(pattern, flags, collation, allowance)
            .map_err(|error| self.failure(error, "invalid regular expression"))?;
        if let Some(cache) = cache {
            #[cfg(test)]
            super::cache::tests::compiled();
            if let Some(admission) = cache.admit(
                pattern,
                compiled.reserved_bytes(),
                self.control.budget(),
                generation.expect("cache generation captured before compilation"),
            ) {
                let regex = compiled.into_retained(|allowance| allowance.retain(admission.memory));
                return Ok(Invocation::Cached {
                    entry: Some(Entry {
                        pattern: admission.pattern,
                        flags,
                        collation: collation_oid,
                        generation: admission.generation,
                        regex,
                    }),
                    cache,
                });
            }
        }
        Ok(Invocation::Fresh(compiled))
    }

    /// Search `input` from code point `start` as `RE_wchar_execute` does, filling one span per slot of `spans`; a failure is `regular expression failed: <explanation>`.
    pub(super) fn execute(
        &self,
        regex: &mut Invocation<'_, '_, '_>,
        input: &[u32],
        start: usize,
        spans: &mut [MatchSpan],
    ) -> Result<bool> {
        self.check()?;
        let result = match regex {
            Invocation::Fresh(regex) => regex.exec(input, start, spans),
            Invocation::Cached { entry, .. } => entry
                .as_mut()
                .expect("active cached expression")
                .regex
                .exec(input, start, spans, &mut self.allowance()),
        };
        result.map_err(|error| self.failure(error, "regular expression failed"))
    }

    /// `CHECK_FOR_INTERRUPTS` between engine operations: the call's own cancellation scopes and the running statement's, which ordinary evaluation, planning and table functions leave to the statement.
    pub(super) fn check(&self) -> Result<()> {
        self.control.check()?;
        crate::expr::check_statement_cancellation()?;
        Ok(())
    }

    fn refuse(&self, error: MemoryError) {
        let first = self.refusal.take().unwrap_or(error);
        self.refusal.set(Some(first));
    }

    fn cancel_requested(&self) -> bool {
        self.control.check_cancellation().is_err()
            || crate::expr::check_statement_cancellation().is_err()
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
            RegexError::Cancelled => match self
                .control
                .check_cancellation()
                .and_then(|()| crate::expr::check_statement_cancellation())
            {
                Err(cancelled) => cancelled.into(),
                Ok(()) => SQLError::Internal(
                    "the regular expression engine stopped for a cancellation the statement does not report".into(),
                ),
            },
            other => diagnostic(other),
        }
    }
}

/// A borrowed ordinary expression or an owned cache entry checked out until this call ends.
pub(super) enum Invocation<'r, 'a, 'c> {
    Fresh(Regex<'r, Allowance<'a, 'c>>),
    Cached {
        entry: Option<Entry>,
        cache: Arc<RegexCache>,
    },
}

impl Invocation<'_, '_, '_> {
    pub(super) fn subexpression_count(&self) -> usize {
        match self {
            Self::Fresh(regex) => regex.subexpression_count(),
            Self::Cached { entry, .. } => entry
                .as_ref()
                .expect("active cached expression")
                .regex
                .subexpression_count(),
        }
    }
}

impl Drop for Invocation<'_, '_, '_> {
    fn drop(&mut self) {
        if let Self::Cached { entry, cache } = self {
            if let Some(entry) = entry.take() {
                cache.keep(entry);
            }
        }
    }
}

/// An error `PostgreSQL` raises with its own SQLSTATE, message and hint.
fn diagnostic(error: RegexError) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: error.sqlstate().unwrap_or("XX000").into(),
        message: error.message().to_string(),
        detail: None,
        hint: error.hint().map(Into::into),
    }
}

/// The size of the code point buffer `regexp.c` allocates for text of `length` bytes, one `pg_wchar` per byte and a terminator, which `palloc` rejects beyond `MaxAllocSize` before anything is converted.
fn code_point_buffer_bytes(length: usize) -> Result<usize> {
    let slots = length.saturating_add(1);
    let bytes = slots.checked_mul(size_of::<u32>()).ok_or_else(|| {
        diagnostic(RegexError::InvalidAllocationProduct(
            slots,
            size_of::<u32>(),
        ))
    })?;
    if bytes > MAX_ALLOC_SIZE {
        return Err(diagnostic(RegexError::InvalidAllocationSize(bytes)));
    }
    Ok(bytes)
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
    code_point_buffer_bytes(text.len())?;
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
