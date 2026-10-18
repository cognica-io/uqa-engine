//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s `parse_re_flags`: a flag string selects the engine's compile options and whether every match is requested.

use super::SingleResult;
use crate::error::{Result, SQLError};
use uqa_pg_regex::{
    CompileFlags, REG_ADVANCED, REG_EXPANDED, REG_EXTENDED, REG_ICASE, REG_NEWLINE, REG_NLANCH,
    REG_NLSTOP, REG_QUOTE,
};

/// The options of one flag string, validated in full before any caller inspects `g`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReFlags {
    /// The `cflags` passed to `pg_regcomp`.
    pub(super) cflags: CompileFlags,
    /// Whether `g` requested every match.
    pub(super) global: bool,
}

impl ReFlags {
    /// The options of an empty flag string: advanced syntax and the first match only.
    pub(super) const ADVANCED: Self = Self {
        cflags: REG_ADVANCED,
        global: false,
    };

    /// Apply each flag in order, so a later flag overrides an earlier one, and report the first character that is not a flag.
    pub(super) fn parse(flags: &str) -> Result<Self> {
        let mut parsed = Self::ADVANCED;
        for flag in flags.chars() {
            match flag {
                'g' => parsed.global = true,
                'b' => parsed.cflags &= !(REG_ADVANCED | REG_EXTENDED | REG_QUOTE),
                'c' => parsed.cflags &= !REG_ICASE,
                // `REG_ADVANCED` includes the `REG_EXTENDED` bit, so clearing it after setting that bit selects basic syntax, exactly as `PostgreSQL` does.
                'e' => {
                    parsed.cflags |= REG_EXTENDED;
                    parsed.cflags &= !(REG_ADVANCED | REG_QUOTE);
                }
                'i' => parsed.cflags |= REG_ICASE,
                'm' | 'n' => parsed.cflags |= REG_NEWLINE,
                'p' => {
                    parsed.cflags |= REG_NLSTOP;
                    parsed.cflags &= !REG_NLANCH;
                }
                'q' => {
                    parsed.cflags |= REG_QUOTE;
                    parsed.cflags &= !(REG_ADVANCED | REG_EXTENDED);
                }
                's' => parsed.cflags &= !REG_NEWLINE,
                't' => parsed.cflags &= !REG_EXPANDED,
                'w' => {
                    parsed.cflags &= !REG_NLSTOP;
                    parsed.cflags |= REG_NLANCH;
                }
                'x' => parsed.cflags |= REG_EXPANDED,
                other => {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!("invalid regular expression option: \"{other}\""),
                    })
                }
            }
        }
        Ok(parsed)
    }

    /// Reject `g` for a function that returns a single result, as each such function does after parsing its flags and before compiling.
    pub(super) fn reject_global(self, caller: SingleResult) -> Result<Self> {
        if !self.global {
            return Ok(self);
        }
        Err(SQLError::Diagnostic {
            sqlstate: "22023".into(),
            message: format!(
                "{}() does not support the \"global\" option",
                caller.function
            ),
            detail: None,
            hint: caller.hint.map(Into::into),
        })
    }

    /// The same options searching for every match, as the functions that count, number or split matches do after rejecting `g`.
    pub(super) const fn every_match(self) -> Self {
        Self {
            cflags: self.cflags,
            global: true,
        }
    }
}

#[cfg(test)]
mod tests;
