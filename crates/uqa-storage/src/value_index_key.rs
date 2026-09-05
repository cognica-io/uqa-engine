//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical B-tree namespaces distinguish column accelerators from named SQL indexes.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ValueIndexKey {
    Column(String),
    Index(String),
}

impl ValueIndexKey {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Column(name) | Self::Index(name) => name,
        }
    }
}

impl From<&str> for ValueIndexKey {
    fn from(name: &str) -> Self {
        Self::Column(name.into())
    }
}

impl From<String> for ValueIndexKey {
    fn from(name: String) -> Self {
        Self::Column(name)
    }
}

impl std::fmt::Display for ValueIndexKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Column(name) => write!(formatter, "column {name}"),
            Self::Index(name) => write!(formatter, "index {name}"),
        }
    }
}

impl rusqlite::ToSql for ValueIndexKey {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        // SQLite's TEXT and BLOB key domains are disjoint, even for identical bytes. Legacy column keys retain their original TEXT representation.
        Ok(ToSqlOutput::Borrowed(match self {
            Self::Column(name) => ValueRef::Text(name.as_bytes()),
            Self::Index(name) => ValueRef::Blob(name.as_bytes()),
        }))
    }
}

impl rusqlite::types::FromSql for ValueIndexKey {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        use rusqlite::types::{FromSqlError, ValueRef};
        match value {
            ValueRef::Text(bytes) => Ok(Self::Column(
                std::str::from_utf8(bytes)
                    .map_err(|error| FromSqlError::Other(Box::new(error)))?
                    .into(),
            )),
            ValueRef::Blob(bytes) => Ok(Self::Index(
                std::str::from_utf8(bytes)
                    .map_err(|error| FromSqlError::Other(Box::new(error)))?
                    .into(),
            )),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}
