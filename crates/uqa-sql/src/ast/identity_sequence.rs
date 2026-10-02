//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The sequence options of an identity column declaration.

use super::{RelationPersistence, SequenceDeclaration};
use crate::SQLError;

/// The sequence options an identity column declaration writes, `GENERATED ... AS IDENTITY (options)`. They create the column's sequence, which keeps them; the column does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentitySequenceDeclaration {
    /// `SEQUENCE NAME`.
    pub name: Option<IdentitySequenceName>,
    /// `LOGGED` or `UNLOGGED`, which the sequence takes instead of its table's persistence.
    pub persistence: Option<RelationPersistence>,
    /// The options `CREATE SEQUENCE` interprets.
    pub sequence: SequenceDeclaration,
    /// The first error interpreting those options raised. `PostgreSQL` interprets them when it creates the sequence, after it has analyzed the whole statement, so the declaration raises it there.
    pub error: Option<DeferredSQLError>,
}

/// The name `SEQUENCE NAME` gives an identity sequence: its schema, when written, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentitySequenceName {
    pub schema: Option<String>,
    pub name: String,
}

/// An error a statement found while compiling, which it raises later, where `PostgreSQL` raises it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredSQLError {
    pub sqlstate: String,
    pub message: String,
}

impl From<&SQLError> for DeferredSQLError {
    fn from(error: &SQLError) -> Self {
        Self {
            sqlstate: error.sqlstate().unwrap_or("XX000").to_string(),
            message: error.to_string(),
        }
    }
}

impl From<DeferredSQLError> for SQLError {
    fn from(error: DeferredSQLError) -> Self {
        Self::Routine {
            sqlstate: error.sqlstate,
            message: error.message,
        }
    }
}
