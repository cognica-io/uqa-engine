//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `regexp_matches`: one capture array per match, advancing as `PostgreSQL`'s `setup_regexp_matches` does.

use super::{compile, flags, next_search_start, query::capture_array, GlobalFlag, LocalRegex};
use crate::{
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// The rows of one `regexp_matches` call. The cursor owns its admitted input and compiled pattern, so a caller can draw each row under its own control.
#[derive(Debug)]
pub struct RegexMatches {
    input: Produced<String>,
    regex: LocalRegex,
    /// The byte offset of the next search, or `None` after the last row.
    next_start: Option<usize>,
}

impl RegexMatches {
    /// Prepare `regexp_matches(string, pattern [, flags])`. A NULL argument yields no cursor: the strict set-returning function returns no rows.
    ///
    /// # Errors
    ///
    /// Reports `PostgreSQL`'s diagnostics for an invalid flag or pattern, an exhausted allowance and cancellation.
    pub fn new(arguments: &[Value], control: &ProductionControl<'_>) -> Result<Option<Self>> {
        control.check()?;
        if !(2..=3).contains(&arguments.len()) {
            return Err(SQLError::TypeMismatch(
                "regexp_matches takes 2 or 3 args".into(),
            ));
        }
        if arguments.iter().any(|value| matches!(value, Value::Null)) {
            return Ok(None);
        }
        let input = value_to_string_with_control(&arguments[0], control)?;
        let pattern = value_to_string_with_control(&arguments[1], control)?;
        let flags = flags(arguments.get(2), control)?;
        let regex = compile(&pattern, &flags, GlobalFlag::Accepted, control)?;
        Ok(Some(Self {
            input,
            regex,
            next_start: Some(0),
        }))
    }

    /// The capture array of the next match, or `None` after the last one.
    ///
    /// # Errors
    ///
    /// Reports an exhausted allowance or cancellation. The cursor stays at the failed match, and rows already returned remain valid.
    pub fn next_row(&mut self, control: &ProductionControl<'_>) -> Result<Option<Produced<Value>>> {
        control.check()?;
        let Some(start) = self.next_start else {
            return Ok(None);
        };
        let Some(captures) = self.regex.captures_at(&self.input, start) else {
            self.next_start = None;
            return Ok(None);
        };
        control.check()?;
        let whole = captures.get(0).ok_or_else(|| {
            SQLError::Internal("regex capture set omitted its mandatory full match".into())
        })?;
        let row = capture_array(&captures, control)?;
        self.next_start = if self.regex.is_global() {
            next_search_start(&self.input, whole)
        } else {
            None
        };
        Ok(Some(row))
    }
}
