//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `regexp_matches`: one capture array per match.

use super::{
    argument_text, flag_text,
    flags::ReFlags,
    match_set::{MatchSet, Search},
};
use crate::error::{Result, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// The rows of one `regexp_matches` call. Like `PostgreSQL`'s first call, preparing the cursor finds every match; the cursor owns the input's code points and the match spans, so a caller can draw each row under its own control.
#[derive(Debug)]
pub struct RegexMatches {
    matches: MatchSet,
    next: usize,
}

impl RegexMatches {
    /// Prepare `regexp_matches(string, pattern [, flags])` under the call's input collation (`PostgreSQL`'s `inputcollid`, zero when indeterminate). A NULL argument yields no cursor: the strict set-returning function returns no rows.
    ///
    /// # Errors
    ///
    /// Reports `PostgreSQL`'s diagnostics for an invalid flag, collation or pattern and for a failed search, an exhausted allowance and cancellation.
    pub fn new(
        arguments: &[Value],
        collation: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        if !(2..=3).contains(&arguments.len()) {
            return Err(SQLError::TypeMismatch(
                "regexp_matches takes 2 or 3 args".into(),
            ));
        }
        if arguments.iter().any(|value| matches!(value, Value::Null)) {
            return Ok(None);
        }
        let input = argument_text(&arguments[0], control)?;
        let pattern = argument_text(&arguments[1], control)?;
        let flags = ReFlags::parse(&flag_text(arguments.get(2), control)?)?;
        let search = Search {
            start: 0,
            subexpressions: true,
            ignore_degenerate: false,
        };
        let matches = MatchSet::find(&input, &pattern, flags, collation, search, control)?;
        Ok(Some(Self { matches, next: 0 }))
    }

    /// The capture array of the next match, or `None` after the last one.
    ///
    /// # Errors
    ///
    /// Reports an exhausted allowance or cancellation. The cursor stays at the failed match, and rows already returned remain valid.
    pub fn next_row(&mut self, control: &ProductionControl<'_>) -> Result<Option<Produced<Value>>> {
        control.check()?;
        if self.next >= self.matches.len() {
            return Ok(None);
        }
        let row = self.matches.row(self.next, control)?;
        self.next += 1;
        Ok(Some(row))
    }
}
