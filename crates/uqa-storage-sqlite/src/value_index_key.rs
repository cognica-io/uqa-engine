//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` TEXT/BLOB encodings preserve column and named-index namespaces.

use std::borrow::Borrow;
use uqa_storage::ValueIndexKey;

pub(crate) struct SQLiteValueIndexKey<T>(pub T);

impl<T: Borrow<ValueIndexKey>> SQLiteValueIndexKey<T> {
    pub(crate) fn as_value_ref(&self) -> rusqlite::types::ValueRef<'_> {
        use rusqlite::types::ValueRef;
        match self.0.borrow() {
            ValueIndexKey::Column(name) => ValueRef::Text(name.as_bytes()),
            ValueIndexKey::Index(name) => ValueRef::Blob(name.as_bytes()),
        }
    }
}

impl<T: Borrow<ValueIndexKey>> rusqlite::ToSql for SQLiteValueIndexKey<T> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(self.as_value_ref()))
    }
}

impl rusqlite::types::FromSql for SQLiteValueIndexKey<ValueIndexKey> {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        use rusqlite::types::{FromSqlError, ValueRef};
        let (bytes, column) = match value {
            ValueRef::Text(bytes) => (bytes, true),
            ValueRef::Blob(bytes) => (bytes, false),
            _ => return Err(FromSqlError::InvalidType),
        };
        let name = std::str::from_utf8(bytes)
            .map_err(|error| FromSqlError::Other(Box::new(error)))?
            .into();
        Ok(Self(if column {
            ValueIndexKey::Column(name)
        } else {
            ValueIndexKey::Index(name)
        }))
    }
}
