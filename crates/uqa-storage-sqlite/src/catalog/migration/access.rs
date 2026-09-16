//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation catalog claims and releases shared by migration and normal catalog operations.

use super::super::{
    params, Catalog, OptionalExtension, RelationIdentity, RelationKind, Result, SQLiteError,
};

impl Catalog {
    pub(in crate::catalog) fn claim_relation(
        conn: &rusqlite::Connection,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let schema_exists = conn
            .query_row(
                "SELECT 1 FROM _schemas WHERE name = ?1",
                params![relation.schema],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let existing = conn
            .query_row(
                "SELECT kind FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if Self::check_relation_claim(relation, kind, schema_exists, existing.as_deref())? {
            conn.execute(
                "INSERT INTO _relations(schema_name, relation_name, kind)
                 VALUES (?1, ?2, ?3)",
                params![relation.schema, relation.name, kind.as_str()],
            )?;
        }
        Ok(())
    }

    pub(in crate::catalog) fn release_relation(
        conn: &rusqlite::Connection,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let existing = conn
            .query_row(
                "SELECT kind FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if Self::check_relation_release(relation, kind, existing.as_deref())? {
            conn.execute(
                "DELETE FROM _relations
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
        }
        Ok(())
    }

    pub(in crate::catalog) fn check_relation_claim(
        relation: &RelationIdentity,
        kind: RelationKind,
        schema_exists: bool,
        existing: Option<&str>,
    ) -> Result<bool> {
        if !schema_exists {
            return Err(SQLiteError::StorageBackend(format!(
                "schema `{}` does not exist for relation `{}`",
                relation.schema,
                relation.qualified_name()
            )));
        }
        match existing {
            Some(existing) if existing == kind.as_str() => Ok(false),
            Some(existing) => Err(SQLiteError::StorageBackend(format!(
                "relation `{}` already exists as {existing}",
                relation.qualified_name()
            ))),
            None => Ok(true),
        }
    }

    pub(in crate::catalog) fn check_relation_release(
        relation: &RelationIdentity,
        kind: RelationKind,
        existing: Option<&str>,
    ) -> Result<bool> {
        if let Some(existing) = existing {
            if existing != kind.as_str() {
                return Err(SQLiteError::StorageBackend(format!(
                    "catalog relation `{}` is {existing}, not {}",
                    relation.qualified_name(),
                    kind.as_str()
                )));
            }
        }
        Ok(existing.is_some())
    }
}
