//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `regexp_split_to_table` and `regexp_split_to_array`: the text between the matches `PostgreSQL`'s `setup_regexp_matches` finds when it ignores degenerate matches.

use super::{compile, flags, next_search_start, plain, string, GlobalFlag, LocalRegex};
use crate::{
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

/// The pieces of one split. The cursor owns its admitted input and compiled pattern, so a caller can draw each piece under its own control.
#[derive(Debug)]
pub struct RegexSplit {
    input: Produced<String>,
    regex: LocalRegex,
    /// The byte offset where the next piece begins: the end of the last match that split the input.
    piece_start: usize,
    /// The end of the last match found, whether or not it split the input.
    previous_match_end: usize,
    /// The byte offset of the next search, or `None` once no further match can split the input.
    search_start: Option<usize>,
    /// Whether the final piece has been returned.
    finished: bool,
}

impl RegexSplit {
    /// Prepare `function(string, pattern [, flags])` for `regexp_split_to_table` or `regexp_split_to_array`. A NULL argument yields no cursor, as both functions are strict.
    ///
    /// # Errors
    ///
    /// Reports `PostgreSQL`'s diagnostics for an invalid flag, the `g` flag and an invalid pattern, an exhausted allowance and cancellation.
    pub fn new(
        function: &'static str,
        arguments: &[Value],
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
        let input = value_to_string_with_control(&arguments[0], control)?;
        let pattern = value_to_string_with_control(&arguments[1], control)?;
        let flags = flags(arguments.get(2), control)?;
        let regex = compile(&pattern, &flags, GlobalFlag::rejected_by(function), control)?;
        Ok(Some(Self {
            input,
            regex,
            piece_start: 0,
            previous_match_end: 0,
            search_start: Some(0),
            finished: false,
        }))
    }

    /// The next piece, or `None` after the last one. A match splits the input unless it is degenerate: empty at the start or end of the input, or empty immediately after the previous match.
    ///
    /// # Errors
    ///
    /// Reports an exhausted allowance or cancellation. The cursor stays before the failed piece, and pieces already returned remain valid.
    pub fn next_piece(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>> {
        control.check()?;
        if self.finished {
            return Ok(None);
        }
        while let Some(start) = self.search_start {
            let Some(matched) = self.regex.find_at(&self.input, start) else {
                self.search_start = None;
                break;
            };
            control.check()?;
            let splits =
                matched.start() < self.input.len() && matched.end() > self.previous_match_end;
            if splits {
                let piece = control.copy_text(&self.input[self.piece_start..matched.start()])?;
                let piece = string(piece, control)?;
                self.piece_start = matched.end();
                self.previous_match_end = matched.end();
                self.search_start = next_search_start(&self.input, matched);
                return Ok(Some(piece));
            }
            self.previous_match_end = matched.end();
            self.search_start = next_search_start(&self.input, matched);
        }
        let piece = string(control.copy_text(&self.input[self.piece_start..])?, control)?;
        self.finished = true;
        Ok(Some(piece))
    }
}

/// `regexp_split_to_array`: every piece of the split as one `text[]` value, or NULL for a NULL argument. The cursor's input copy and compiled pattern are released before the array is returned.
pub(super) fn array(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let Some(mut split) = RegexSplit::new("regexp_split_to_array", args, control)? else {
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
