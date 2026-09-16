//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog records use the managed session's committed/private view and atomic staging boundary.

mod analyzers;
mod columns;
mod foreign;
pub(super) mod graph;
mod indexes;
mod relations;
mod sequences;
mod stats;
mod tables;
mod views;
pub(super) use analyzers::FieldWrite;
pub(super) use relations::RelationRecord;

use super::{Catalog, Result, SQLiteError, SchemaRow};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordOwner, NativeSnapshot};
use rusqlite::types::ValueRef;

pub(super) enum NativeLookup<T> {
    Unbound,
    Value(T),
}

pub(super) fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}
pub(super) fn optional_text(value: Option<&str>) -> ValueRef<'_> {
    value.map_or(ValueRef::Null, text)
}
pub(super) fn string(value: ValueRef<'_>) -> Result<String> {
    value.as_str().map(str::to_owned).map_err(|_| {
        SQLiteError::StorageBackend("native catalog text column has another storage class".into())
    })
}

impl Catalog {
    pub(super) fn read_native<R>(
        &self,
        operation: impl FnOnce(&NativeSnapshot) -> Result<R>,
    ) -> Result<Option<R>> {
        self.conn
            .native_snapshot()?
            .map(|snapshot| operation(&snapshot))
            .transpose()
    }

    pub(super) fn put_native_named(
        &self,
        family: Family,
        row: &[ValueRef<'_>],
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            if family == Family::Metadata {
                if let Some(graph) = row[0]
                    .as_str()
                    .ok()
                    .and_then(|name| name.strip_prefix("graph_label_registry::"))
                {
                    graph::paths::invalidate_graph(snapshot, batch, graph)?;
                }
            }
            if family == Family::Metadata && row[0] == text("schema_version") {
                let existing = snapshot.read_row(
                    family,
                    NativeRecordOwner::Database(snapshot.database),
                    &[row[0]],
                    |previous| Ok(previous[1] == row[1]),
                )?;
                if existing != Some(true) {
                    return Err(SQLiteError::StorageBackend(
                        "native catalog format version is immutable".into(),
                    ));
                }
            }
            snapshot.put_row(
                batch,
                family,
                NativeRecordOwner::Database(snapshot.database),
                row,
            )
        })
    }

    pub(super) fn get_native_named(
        &self,
        family: Family,
        name: &str,
        column: usize,
    ) -> Result<NativeLookup<Option<String>>> {
        let value = self.read_native(|snapshot| {
            snapshot.read_row(
                family,
                NativeRecordOwner::Database(snapshot.database),
                &[text(name)],
                |row| string(row[column]),
            )
        })?;
        Ok(value.map_or(NativeLookup::Unbound, NativeLookup::Value))
    }

    pub(super) fn load_native_named(
        &self,
        family: Family,
        column: usize,
        nonnull: bool,
    ) -> Result<Option<Vec<(String, String)>>> {
        self.read_native(|snapshot| {
            let mut rows = Vec::new();
            snapshot.visit_rows(
                family,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    if !nonnull || row[column] != ValueRef::Null {
                        rows.push((string(row[0])?, string(row[column])?));
                    }
                    Ok(())
                },
            )?;
            Ok(rows)
        })
    }

    pub(super) fn drop_native_named(&self, family: Family, name: &str) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.delete_prefix(
                batch,
                family,
                NativeRecordOwner::Database(snapshot.database),
                &[text(name)],
            )
        })
    }

    pub(super) fn load_native_schemas(&self) -> Result<Option<Vec<SchemaRow>>> {
        self.read_native(|snapshot| {
            let mut schemas = Vec::new();
            snapshot.visit_rows(
                Family::Schemas,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    let acl = if row[2] == ValueRef::Null {
                        None
                    } else {
                        Some(serde_json::from_str(&string(row[2])?)?)
                    };
                    schemas.push(SchemaRow {
                        name: string(row[0])?,
                        role_owner: string(row[1])?,
                        acl,
                    });
                    Ok(())
                },
            )?;
            Ok(schemas)
        })
    }

    pub(super) fn drop_native_schema(&self, name: &str) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            let mut used = false;
            snapshot.visit_rows(Family::Relations, Some(owner), &[text(name)], |_| {
                used = true;
                Ok(())
            })?;
            if used {
                return Err(SQLiteError::StorageBackend(format!(
                    "schema `{name}` still owns catalog relations"
                )));
            }
            snapshot.delete_prefix(batch, Family::Schemas, owner, &[text(name)])
        })
    }
}
