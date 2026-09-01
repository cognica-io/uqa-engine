//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation persistence, view options, and sequence lifecycle nodes.

use serde::{Deserialize, Serialize};

/// `PostgreSQL`'s `pg_class.relpersistence` contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationPersistence {
    #[default]
    Permanent,
    Unlogged,
    Temporary,
}

impl RelationPersistence {
    #[must_use]
    pub const fn catalog_code(self) -> &'static str {
        match self {
            Self::Permanent => "p",
            Self::Unlogged => "u",
            Self::Temporary => "t",
        }
    }
}

/// `ON COMMIT` behavior retained with a temporary table definition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnCommitAction {
    #[default]
    PreserveRows,
    DeleteRows,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlterViewKind {
    View,
    MaterializedView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlterViewOptionsAction {
    Set(Vec<(String, String)>),
    Reset(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlterViewOptionsStmt {
    pub name: String,
    pub kind: AlterViewKind,
    pub if_exists: bool,
    pub action: AlterViewOptionsAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSequence {
    pub name: String,
    pub if_not_exists: bool,
    pub start: i64,
    pub increment: i64,
    #[serde(default)]
    pub persistence: RelationPersistence,
    #[serde(default)]
    pub data_type: SequenceDataType,
    /// Concrete bounds are written by current compilers. `None` is retained for backward-compatible plans and means the `PostgreSQL` default for the declared type and increment direction.
    #[serde(default)]
    pub min_value: Option<i64>,
    #[serde(default)]
    pub max_value: Option<i64>,
    #[serde(default)]
    pub cycle: bool,
    #[serde(default = "default_sequence_cache_size")]
    pub cache_size: i64,
    #[serde(default)]
    pub ownership: SequenceOwnership,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceDataType {
    SmallInt,
    Integer,
    #[default]
    BigInt,
}

const fn default_sequence_cache_size() -> i64 {
    1
}

impl SequenceDataType {
    #[must_use]
    pub const fn sql_name(self) -> &'static str {
        match self {
            Self::SmallInt => "smallint",
            Self::Integer => "integer",
            Self::BigInt => "bigint",
        }
    }

    #[must_use]
    pub const fn bounds(self) -> (i64, i64) {
        match self {
            Self::SmallInt => (i16::MIN as i64, i16::MAX as i64),
            Self::Integer => (i32::MIN as i64, i32::MAX as i64),
            Self::BigInt => (i64::MIN, i64::MAX),
        }
    }
}

/// Physical restart action carried by `ALTER SEQUENCE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceRestart {
    /// No `RESTART` clause was specified.
    #[default]
    Unchanged,
    /// Bare `RESTART`; allocate the configured start value next.
    FromStart,
    /// `RESTART WITH value`; allocate the supplied value next.
    With(i64),
}

/// `ALTER SEQUENCE` bound action, distinguishing omission from `NO MINVALUE` or `NO MAXVALUE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceBound {
    #[default]
    Unchanged,
    Default,
    Value(i64),
}

/// `OWNED BY` action carried by `CREATE SEQUENCE` and `ALTER SEQUENCE`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceOwnership {
    /// No ownership clause was specified. On `CREATE SEQUENCE` this creates an unowned sequence; on `ALTER SEQUENCE` it preserves the current dependency.
    #[default]
    Unchanged,
    /// Explicit `OWNED BY NONE`.
    Unowned,
    /// A table relation and one of its columns. The engine resolves both names to stable catalog object identities before persisting the dependency.
    Column { table: String, column: String },
}

fn deserialize_sequence_restart<'de, D>(deserializer: D) -> Result<SequenceRestart, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    enum Current {
        Unchanged,
        FromStart,
        With(i64),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Representation {
        Current(Current),
        // Before SequenceRestart existed this field was
        // Option<Option<i64>>, serialized as null or an integer.
        Legacy(Option<i64>),
    }

    Ok(match Representation::deserialize(deserializer)? {
        Representation::Current(Current::Unchanged) | Representation::Legacy(None) => {
            SequenceRestart::Unchanged
        }
        Representation::Current(Current::FromStart) => SequenceRestart::FromStart,
        Representation::Current(Current::With(value)) | Representation::Legacy(Some(value)) => {
            SequenceRestart::With(value)
        }
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlterSequence {
    pub name: String,
    /// `ALTER SEQUENCE IF EXISTS` suppresses only a missing sequence.
    #[serde(default)]
    pub if_exists: bool,
    /// `RESTART [WITH n]`, preserving omitted, bare, and explicit forms.
    #[serde(default, deserialize_with = "deserialize_sequence_restart")]
    pub restart: SequenceRestart,
    pub increment: Option<i64>,
    pub start: Option<i64>,
    #[serde(default)]
    pub data_type: Option<SequenceDataType>,
    #[serde(default)]
    pub min_value: SequenceBound,
    #[serde(default)]
    pub max_value: SequenceBound,
    #[serde(default)]
    pub cycle: Option<bool>,
    pub cache_size: Option<i64>,
    #[serde(default)]
    pub ownership: SequenceOwnership,
    /// `SET LOGGED` or `SET UNLOGGED`. Temporary is never a valid requested target state.
    #[serde(default)]
    pub persistence: Option<RelationPersistence>,
}
