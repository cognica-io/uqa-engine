//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation selection shared by typed expressions and `PostgreSQL` catalog encoding.

use super::{builtin_collation_name, DEFAULT_COLLATION_OID};
use crate::SQLError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Derivation {
    #[default]
    None,
    Implicit(i64),
    Conflict(i64, i64),
    Explicit(i64),
    /// Keep the first ordered conflict until the containing query's type checks finish.
    InvalidExplicit(i64, i64),
}

impl Derivation {
    pub(crate) fn implicit(oid: i64) -> Self {
        if oid == 0 {
            Self::None
        } else {
            Self::Implicit(oid)
        }
    }

    pub(crate) fn oid(self) -> i64 {
        match self {
            Self::None | Self::Conflict(..) | Self::InvalidExplicit(..) => 0,
            Self::Implicit(oid) | Self::Explicit(oid) => oid,
        }
    }

    /// Combine in child visitation order without reporting a collation error before a later type error.
    pub(crate) fn combine(self, next: Self) -> Self {
        match (self, next) {
            (value @ Self::InvalidExplicit(..), _) | (_, value @ Self::InvalidExplicit(..)) => {
                value
            }
            (Self::Explicit(left), Self::Explicit(right)) if left != right => {
                Self::InvalidExplicit(left, right)
            }
            (value @ Self::Explicit(_), _) | (_, value @ Self::Explicit(_)) => value,
            (value @ Self::Conflict(..), _) | (_, value @ Self::Conflict(..)) => value,
            (Self::None, value) | (value, Self::None) => value,
            (Self::Implicit(left), Self::Implicit(right)) => {
                if left == right || right == DEFAULT_COLLATION_OID {
                    Self::Implicit(left)
                } else if left == DEFAULT_COLLATION_OID {
                    Self::Implicit(right)
                } else {
                    Self::Conflict(left, right)
                }
            }
        }
    }

    pub(crate) fn merge(self, next: Self) -> Result<Self, SQLError> {
        let result = self.combine(next);
        result.validate()?;
        Ok(result)
    }

    pub(crate) fn validate(self) -> Result<(), SQLError> {
        let Self::InvalidExplicit(left, right) = self else {
            return Ok(());
        };
        let name = |oid| {
            builtin_collation_name(oid).ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "unknown collation identity".into(),
            })
        };
        Err(SQLError::Routine {
            sqlstate: "42P21".into(),
            message: format!(
                "collation mismatch between explicit collations \"{}\" and \"{}\"",
                name(left)?,
                name(right)?,
            ),
        })
    }

    pub(crate) fn error_only(self) -> Self {
        match self {
            Self::InvalidExplicit(..) => self,
            _ => Self::None,
        }
    }

    pub(crate) fn explicit(self, oid: i64) -> Self {
        match self {
            Self::InvalidExplicit(..) => self,
            _ => Self::Explicit(oid),
        }
    }

    pub(crate) fn result(self, declared: i64) -> Self {
        if matches!(self, Self::InvalidExplicit(..)) {
            self
        } else if declared == 0 {
            Self::None
        } else if self == Self::None {
            Self::Implicit(declared)
        } else {
            self
        }
    }
}
