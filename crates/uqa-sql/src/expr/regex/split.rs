//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `regexp_split_to_table` and `regexp_split_to_array`: the text between the matches `PostgreSQL`'s `setup_regexp_matches` finds when it ignores degenerate matches.

use super::{
    argument_text, flag_text,
    flags::ReFlags,
    match_set::{MatchSet, Search},
    plain, string, SingleResult,
};
use crate::error::{Result, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

/// The pieces of one split. Preparing the cursor finds every match; the cursor owns the input's code points and the match spans, so a caller can draw each piece under its own control.
#[derive(Debug)]
pub struct RegexSplit {
    matches: MatchSet,
    /// The piece returned next: the text before match `next`, or after the last match once `next` equals the match count.
    next: usize,
}

impl RegexSplit {
    /// Prepare `function(string, pattern [, flags])` for `regexp_split_to_table` or `regexp_split_to_array` under the call's input collation (`PostgreSQL`'s `inputcollid`, zero when indeterminate). A NULL argument yields no cursor, as both functions are strict.
    ///
    /// # Errors
    ///
    /// Reports `PostgreSQL`'s diagnostics for an invalid flag, the `g` flag, an invalid collation or pattern and a failed search, an exhausted allowance and cancellation.
    pub fn new(
        function: &'static str,
        arguments: &[Value],
        collation: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        if !(2..=3).contains(&arguments.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "{function} takes 2 or 3 args"
            )));
        }
        if arguments.iter().any(|value| matches!(value, Value::Null)) {
            return Ok(None);
        }
        let input = argument_text(&arguments[0], control)?;
        let pattern = argument_text(&arguments[1], control)?;
        let flags = ReFlags::parse(&flag_text(arguments.get(2), control)?)?
            .reject_global(SingleResult::named(function))?
            .every_match();
        let search = Search {
            start: 0,
            subexpressions: false,
            ignore_degenerate: true,
        };
        let matches = MatchSet::find(&input, &pattern, flags, collation, search, control)?;
        Ok(Some(Self { matches, next: 0 }))
    }

    /// The next piece, or `None` after the last one, `build_regexp_split_result`.
    ///
    /// # Errors
    ///
    /// Reports an exhausted allowance or cancellation. The cursor stays before the failed piece, and pieces already returned remain valid.
    pub fn next_piece(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>> {
        control.check()?;
        let count = self.matches.len();
        if self.next > count {
            return Ok(None);
        }
        let start = if self.next == 0 {
            Some(0)
        } else {
            self.matches.span(self.next - 1, 0).end()
        };
        let end = if self.next == count {
            Some(self.matches.input_len())
        } else {
            self.matches.span(self.next, 0).start()
        };
        let (Some(start), Some(end)) = (start, end) else {
            return Err(SQLError::Internal(
                "a regular expression split lost a match boundary".into(),
            ));
        };
        let piece = string(self.matches.slice(start..end, control)?, control)?;
        self.next += 1;
        Ok(Some(piece))
    }
}

/// `regexp_split_to_array`: every piece of the split as one `text[]` value, or NULL for a NULL argument. The cursor's code points and spans are released before the array is returned.
pub(super) fn array(
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let Some(mut split) = RegexSplit::new("regexp_split_to_array", args, collation, control)?
    else {
        return plain(Value::Null, control);
    };
    let mut pieces = ProductionVec::new(*control);
    while let Some(piece) = split.next_piece(control)? {
        pieces.push_produced(piece)?;
    }
    drop(split);
    let array = ArrayValue::try_new_with_control(pieces.finish()?, control)?
        .ok_or_else(|| SQLError::TypeMismatch("invalid regexp_split_to_array result".into()))?;
    let (array, memory) = array.into_parts();
    control
        .finish(Value::Array(array), memory)
        .map_err(Into::into)
}
