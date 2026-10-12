//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation selection shared by typed expressions and `PostgreSQL` catalog encoding.

use super::{builtin_collation_name, DEFAULT_COLLATION_OID};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
use crate::{ColumnType, SQLError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Derivation {
    #[default]
    None,
    Implicit(i64),
    Conflict(i64, i64),
    Explicit(i64),
    /// Keep the first ordered conflict until the containing query's type checks finish.
    InvalidExplicit(i64, i64),
    /// A comparison key requires a determinate collation after type analysis finishes.
    InvalidImplicit(i64, i64),
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
            Self::None
            | Self::Conflict(..)
            | Self::InvalidExplicit(..)
            | Self::InvalidImplicit(..) => 0,
            Self::Implicit(oid) | Self::Explicit(oid) => oid,
        }
    }

    /// Combine in child visitation order without reporting a collation error before a later type error.
    pub(crate) fn combine(self, next: Self) -> Self {
        match (self, next) {
            (value @ (Self::InvalidExplicit(..) | Self::InvalidImplicit(..)), _)
            | (_, value @ (Self::InvalidExplicit(..) | Self::InvalidImplicit(..))) => value,
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
        if let Self::InvalidImplicit(left, right) = self {
            return Self::Conflict(left, right).common_oid(false).map(|_| ());
        }
        let Self::InvalidExplicit(left, right) = self else {
            return Ok(());
        };
        Err(SQLError::Routine {
            sqlstate: "42P21".into(),
            message: format!(
                "collation mismatch between explicit collations \"{}\" and \"{}\"",
                collation_name(left)?,
                collation_name(right)?,
            ),
        })
    }

    /// Result columns may retain an implicit conflict only when their containing operation permits an indeterminate collation.
    pub(crate) fn common_oid(self, allow_indeterminate: bool) -> Result<i64, SQLError> {
        self.validate()?;
        if let Self::Conflict(left, right) = self {
            if !allow_indeterminate {
                return Err(SQLError::Diagnostic {
                    sqlstate: "42P21".into(),
                    message: format!(
                        "collation mismatch between implicit collations \"{}\" and \"{}\"",
                        collation_name(left)?, collation_name(right)?,
                    ),
                    detail: None,
                    hint: Some("You can choose the collation by applying the COLLATE clause to one or both expressions.".into()),
                });
            }
        }
        Ok(self.oid())
    }

    pub(crate) fn error_only(self) -> Self {
        match self {
            Self::InvalidExplicit(..) | Self::InvalidImplicit(..) => self,
            _ => Self::None,
        }
    }

    pub(crate) fn explicit(self, oid: i64) -> Self {
        match self {
            Self::InvalidExplicit(..) | Self::InvalidImplicit(..) => self,
            _ => Self::Explicit(oid),
        }
    }

    pub(crate) fn result(self, declared: i64) -> Self {
        if self.error_only() != Self::None {
            self
        } else if declared == 0 {
            Self::None
        } else if self == Self::None {
            Self::Implicit(declared)
        } else {
            self
        }
    }

    /// A key turns its otherwise permitted implicit conflict into a deferred diagnostic.
    pub(crate) fn key(self) -> Self {
        match self {
            Self::Conflict(left, right) => Self::InvalidImplicit(left, right),
            _ => self,
        }
    }

    /// A domain coercion selects its declaration only after visiting its input.
    pub(crate) fn domain(self, declared: i64) -> Self {
        if declared == DEFAULT_COLLATION_OID {
            self.result(declared)
        } else {
            self.error_only().combine(Self::implicit(declared))
        }
    }

    /// Derive metadata from the coercion of already analyzed types, without reanalyzing their expressions.
    pub(crate) fn coerce(self, source: Option<&ColumnType>, target: &ColumnType) -> Self {
        let declared = pg_type_collation_oid(target);
        let Some(source) = source else {
            return self.result(declared);
        };
        if pg_type_oid(source) == pg_type_oid(target)
            && pg_type_modifier(source) == pg_type_modifier(target)
        {
            return self;
        }
        if let ColumnType::Domain { base, .. } = target {
            return self.coerce(Some(source), base).domain(declared);
        }
        if let (ColumnType::Array(_), ColumnType::Array(_)) =
            (crate::type_resolution::base_type(source), target)
        {
            // ArrayCoerceExpr visits both the array and an independently coerced element placeholder.
            let element = Self::None.coerce(
                crate::type_resolution::array_element_type(source),
                crate::type_resolution::array_element_type(target).expect("array element"),
            );
            return self.combine(element).result(declared);
        }
        self.result(declared)
    }
}

fn collation_name(oid: i64) -> Result<&'static str, SQLError> {
    builtin_collation_name(oid).ok_or_else(|| SQLError::Routine {
        sqlstate: "XX000".into(),
        message: "unknown collation identity".into(),
    })
}
