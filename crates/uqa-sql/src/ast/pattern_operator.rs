//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native pattern syntax retains its operator identity independently of routine names.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternOperator {
    Like,
    NotLike,
    ILike,
    NotILike,
    Regex,
    NotRegex,
    RegexInsensitive,
    NotRegexInsensitive,
}

impl PatternOperator {
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "~~" => Self::Like,
            "!~~" => Self::NotLike,
            "~~*" => Self::ILike,
            "!~~*" => Self::NotILike,
            "~" => Self::Regex,
            "!~" => Self::NotRegex,
            "~*" => Self::RegexInsensitive,
            "!~*" => Self::NotRegexInsensitive,
            _ => return None,
        })
    }

    /// The selected binary operator after any SQL escape helper.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Like => "~~",
            Self::NotLike => "!~~",
            Self::ILike => "~~*",
            Self::NotILike => "!~~*",
            Self::Regex => "~",
            Self::NotRegex => "!~",
            Self::RegexInsensitive => "~*",
            Self::NotRegexInsensitive => "!~*",
        }
    }

    #[must_use]
    pub const fn syntax(self) -> &'static str {
        match self {
            Self::Like => "LIKE",
            Self::NotLike => "NOT LIKE",
            Self::ILike => "ILIKE",
            Self::NotILike => "NOT ILIKE",
            _ => self.symbol(),
        }
    }

    #[must_use]
    pub const fn is_like(self) -> bool {
        matches!(
            self,
            Self::Like | Self::NotLike | Self::ILike | Self::NotILike
        )
    }

    #[must_use]
    pub const fn case_insensitive(self) -> bool {
        matches!(
            self,
            Self::ILike | Self::NotILike | Self::RegexInsensitive | Self::NotRegexInsensitive
        )
    }

    #[must_use]
    pub const fn negated(self) -> bool {
        matches!(
            self,
            Self::NotLike | Self::NotILike | Self::NotRegex | Self::NotRegexInsensitive
        )
    }
}
