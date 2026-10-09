//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// Operation selected for one call of an `anyenum` support function. The bound call carries the concrete enum type, because `enum_first`, `enum_last` and `enum_range` read only their argument's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnumFunctionOperation {
    First,
    Last,
    Range,
    BoundedRange,
    Compare,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Smaller,
    Larger,
    Hash,
    ExtendedHash,
}

impl EnumFunctionOperation {
    /// Ordering support calls retain the actual enum type selected on their first slow comparison.
    #[must_use]
    pub const fn uses_comparison_state(self) -> bool {
        matches!(
            self,
            Self::Compare
                | Self::Less
                | Self::Greater
                | Self::LessEqual
                | Self::GreaterEqual
                | Self::Smaller
                | Self::Larger
        )
    }

    /// Resolve a local `pg_catalog` routine name and argument count to its operation.
    #[must_use]
    pub fn from_call(name: &str, argument_count: usize) -> Option<Self> {
        let local = match name.split_once('.') {
            Some((namespace, local)) if namespace.eq_ignore_ascii_case("pg_catalog") => local,
            Some(_) => return None,
            None => name,
        };
        let operation = [
            ("enum_first", 1, Self::First),
            ("enum_last", 1, Self::Last),
            ("enum_range", 1, Self::Range),
            ("enum_range", 2, Self::BoundedRange),
            ("enum_cmp", 2, Self::Compare),
            ("enum_eq", 2, Self::Equal),
            ("enum_ne", 2, Self::NotEqual),
            ("enum_lt", 2, Self::Less),
            ("enum_gt", 2, Self::Greater),
            ("enum_le", 2, Self::LessEqual),
            ("enum_ge", 2, Self::GreaterEqual),
            ("enum_smaller", 2, Self::Smaller),
            ("enum_larger", 2, Self::Larger),
            ("hashenum", 1, Self::Hash),
            ("hashenumextended", 2, Self::ExtendedHash),
        ]
        .into_iter()
        .find(|(candidate, arity, _)| {
            *arity == argument_count && local.eq_ignore_ascii_case(candidate)
        })?
        .2;
        Some(operation)
    }

    /// Whether any overload of this routine name is an enum support function.
    #[must_use]
    pub fn is_routine_name(name: &str) -> bool {
        (1..=2).any(|count| Self::from_call(name, count).is_some())
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::First => "pg_catalog.enum_first",
            Self::Last => "pg_catalog.enum_last",
            Self::Range | Self::BoundedRange => "pg_catalog.enum_range",
            Self::Compare => "pg_catalog.enum_cmp",
            Self::Equal => "pg_catalog.enum_eq",
            Self::NotEqual => "pg_catalog.enum_ne",
            Self::Less => "pg_catalog.enum_lt",
            Self::Greater => "pg_catalog.enum_gt",
            Self::LessEqual => "pg_catalog.enum_le",
            Self::GreaterEqual => "pg_catalog.enum_ge",
            Self::Smaller => "pg_catalog.enum_smaller",
            Self::Larger => "pg_catalog.enum_larger",
            Self::Hash => "pg_catalog.hashenum",
            Self::ExtendedHash => "pg_catalog.hashenumextended",
        }
    }

    /// `enum_first`, `enum_last` and both `enum_range` forms accept NULL arguments; every other support function is strict.
    #[must_use]
    pub const fn is_strict(self) -> bool {
        !matches!(
            self,
            Self::First | Self::Last | Self::Range | Self::BoundedRange
        )
    }

    /// The zero-based argument positions declared `anyenum`; `hashenumextended` takes a trailing `int8` seed.
    #[must_use]
    pub const fn enum_argument_count(self) -> usize {
        match self {
            Self::First | Self::Last | Self::Range | Self::Hash | Self::ExtendedHash => 1,
            _ => 2,
        }
    }
}
