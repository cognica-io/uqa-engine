//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s `REG_*` compile flags and error codes from `src/include/regex/regex.h`.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// Compile flags for [`crate::Regex::compile`], a set of the public `REG_*` compile bits.
///
/// The bitwise operators keep the set within those bits, so `flags & !REG_ICASE` clears one option as `PostgreSQL`'s `parse_re_flags` does with `&= ~REG_ICASE`. The engine's debugging and internal bits cannot be expressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CompileFlags(i32);

impl CompileFlags {
    const PUBLIC: i32 = 0o377;

    /// The `cflags` value passed to `pg_regcomp`.
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for CompileFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for CompileFlags {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl BitAnd for CompileFlags {
    type Output = Self;

    fn bitand(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl BitAndAssign for CompileFlags {
    fn bitand_assign(&mut self, other: Self) {
        self.0 &= other.0;
    }
}

impl Not for CompileFlags {
    type Output = Self;

    fn not(self) -> Self {
        Self(!self.0 & Self::PUBLIC)
    }
}

/// Basic regular expressions.
pub const REG_BASIC: CompileFlags = CompileFlags(0o0);
/// Extended regular expressions.
pub const REG_EXTENDED: CompileFlags = CompileFlags(0o1);
/// Advanced features in extended regular expressions.
pub const REG_ADVF: CompileFlags = CompileFlags(0o2);
/// Advanced regular expressions, `PostgreSQL`'s default flavor.
pub const REG_ADVANCED: CompileFlags = CompileFlags(0o3);
/// The pattern is a literal string.
pub const REG_QUOTE: CompileFlags = CompileFlags(0o4);
/// Historical synonym for [`REG_QUOTE`].
pub const REG_NOSPEC: CompileFlags = REG_QUOTE;
/// Case-insensitive matching.
pub const REG_ICASE: CompileFlags = CompileFlags(0o10);
/// The caller does not need subexpression spans.
pub const REG_NOSUB: CompileFlags = CompileFlags(0o20);
/// Expanded syntax: white space and comments in the pattern are ignored.
pub const REG_EXPANDED: CompileFlags = CompileFlags(0o40);
/// Newline does not match `.` or a negated bracket expression.
pub const REG_NLSTOP: CompileFlags = CompileFlags(0o100);
/// `^` and `$` also match after and before a newline.
pub const REG_NLANCH: CompileFlags = CompileFlags(0o200);
/// Newline-sensitive matching: [`REG_NLSTOP`] and [`REG_NLANCH`].
pub const REG_NEWLINE: CompileFlags = CompileFlags(0o300);

/// No errors detected.
pub const REG_OKAY: i32 = 0;
/// Failed to match.
pub const REG_NOMATCH: i32 = 1;
/// Invalid regexp.
pub const REG_BADPAT: i32 = 2;
/// Invalid collating element.
pub const REG_ECOLLATE: i32 = 3;
/// Invalid character class.
pub const REG_ECTYPE: i32 = 4;
/// Invalid escape sequence.
pub const REG_EESCAPE: i32 = 5;
/// Invalid backreference number.
pub const REG_ESUBREG: i32 = 6;
/// Brackets `[]` not balanced.
pub const REG_EBRACK: i32 = 7;
/// Parentheses `()` not balanced.
pub const REG_EPAREN: i32 = 8;
/// Braces `{}` not balanced.
pub const REG_EBRACE: i32 = 9;
/// Invalid repetition count(s).
pub const REG_BADBR: i32 = 10;
/// Invalid character range.
pub const REG_ERANGE: i32 = 11;
/// Out of memory, including a reservation refused by [`crate::RegexResources::reserve`].
pub const REG_ESPACE: i32 = 12;
/// Quantifier operand invalid.
pub const REG_BADRPT: i32 = 13;
/// "Cannot happen": an internal engine error.
pub const REG_ASSERT: i32 = 15;
/// Invalid argument to a regex function.
pub const REG_INVARG: i32 = 16;
/// Character widths of regex and string differ.
pub const REG_MIXED: i32 = 17;
/// Invalid embedded option.
pub const REG_BADOPT: i32 = 18;
/// Regular expression is too complex, including a stack refused by [`crate::RegexResources::stack_too_deep`].
pub const REG_ETOOBIG: i32 = 19;
/// Too many colors.
pub const REG_ECOLORS: i32 = 20;
