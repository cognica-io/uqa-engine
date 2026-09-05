//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Separate tuple-version metadata from user document fields.

use super::super::super::{Catalog, Result, SQLiteError};
use crate::catalog::RelationIdentity;
use std::collections::BTreeSet;

const LEGACY_SYSTEM_XMIN: &str = "\0uqa.system.xmin";
const LEGACY_USER_XMIN_MARKER: &str = "\0uqa.user.xmin";
const MIGRATION_PAGE_SIZE: i64 = 512;

struct PersistedDocument {
    table_name: String,
    doc_id: i64,
    body: String,
    tuple_xmin: Option<i64>,
}

struct MigratedDocument {
    table_name: String,
    doc_id: i64,
    body: String,
    tuple_xmin: Option<i64>,
}

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_documents")?.unwrap_or_default();
    if !columns.contains_key("tuple_xmin") {
        tx.execute_batch(
            "ALTER TABLE _documents
             ADD COLUMN tuple_xmin INTEGER
             CHECK (tuple_xmin IS NULL OR tuple_xmin BETWEEN 0 AND 4294967295);",
        )?;
    }

    let (known_tables, declared_xmin_tables) = catalog_xmin_tables(tx)?;
    let mut update = tx.prepare_cached(
        "UPDATE _documents
         SET body = ?3, tuple_xmin = ?4
         WHERE table_name = ?1 AND doc_id = ?2",
    )?;
    let mut after = None;
    loop {
        let page = document_page(tx, after.as_ref())?;
        let Some(last) = page.last() else {
            break;
        };
        after = Some((last.table_name.clone(), last.doc_id));
        for document in page {
            let Some(document) = migrate_document(document, &known_tables, &declared_xmin_tables)?
            else {
                continue;
            };
            update.execute(rusqlite::params![
                document.table_name,
                document.doc_id,
                document.body,
                document.tuple_xmin,
            ])?;
        }
    }
    Ok(())
}

fn document_page(
    tx: &rusqlite::Transaction<'_>,
    after: Option<&(String, i64)>,
) -> Result<Vec<PersistedDocument>> {
    let (after_table, after_doc_id) =
        after.map_or((None, 0), |(table, doc_id)| (Some(table.as_str()), *doc_id));
    let mut statement = tx.prepare(
        "SELECT table_name, doc_id, body, tuple_xmin
         FROM _documents
         WHERE ?1 IS NULL
            OR table_name > ?1
            OR (table_name = ?1 AND doc_id > ?2)
         ORDER BY table_name, doc_id
         LIMIT ?3",
    )?;
    let documents = statement
        .query_map(
            rusqlite::params![after_table, after_doc_id, MIGRATION_PAGE_SIZE],
            |row| {
                Ok(PersistedDocument {
                    table_name: row.get(0)?,
                    doc_id: row.get(1)?,
                    body: row.get(2)?,
                    tuple_xmin: row.get(3)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(documents)
}

fn migrate_document(
    document: PersistedDocument,
    known_tables: &BTreeSet<String>,
    declared_xmin_tables: &BTreeSet<String>,
) -> Result<Option<MigratedDocument>> {
    let PersistedDocument {
        table_name,
        doc_id,
        body,
        tuple_xmin: stored_xmin,
    } = document;
    let mut fields = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&body)?;
    let legacy_xmin = fields.remove(LEGACY_SYSTEM_XMIN);
    let user_marker = fields.remove(LEGACY_USER_XMIN_MARKER);
    let user_xmin = user_marker
        .as_ref()
        .is_some_and(|value| value == &serde_json::Value::Bool(true));
    let tuple_xmin = match (stored_xmin, legacy_xmin.as_ref()) {
        (Some(stored), _) => Some(stored),
        (None, Some(serde_json::Value::Number(value))) => {
            let value = value.as_u64().ok_or_else(|| {
                SQLiteError::StorageBackend(format!(
                    "document `{table_name}` row {doc_id} has an invalid legacy tuple xmin"
                ))
            })?;
            Some(i64::try_from(value).map_err(|_| {
                SQLiteError::StorageBackend(format!(
                    "document `{table_name}` row {doc_id} has an out-of-range legacy tuple xmin"
                ))
            })?)
        }
        (None, Some(_)) => {
            return Err(SQLiteError::StorageBackend(format!(
                "document `{table_name}` row {doc_id} has a non-integer legacy tuple xmin"
            )))
        }
        (None, None) => None,
    };
    if tuple_xmin.is_some_and(|value| !(0..=4_294_967_295).contains(&value)) {
        return Err(SQLiteError::StorageBackend(format!(
            "document `{table_name}` row {doc_id} has an out-of-range tuple xmin"
        )));
    }
    let remove_public_xmin = !user_xmin
        && known_tables.contains(&table_name)
        && !declared_xmin_tables.contains(&table_name)
        && legacy_xmin
            .as_ref()
            .is_some_and(|legacy| fields.get("xmin") == Some(legacy));
    if remove_public_xmin {
        fields.remove("xmin");
    }
    if legacy_xmin.is_none() && user_marker.is_none() && tuple_xmin == stored_xmin {
        return Ok(None);
    }
    Ok(Some(MigratedDocument {
        table_name,
        doc_id,
        body: serde_json::to_string(&fields)?,
        tuple_xmin,
    }))
}

fn catalog_xmin_tables(
    tx: &rusqlite::Transaction<'_>,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut statement = tx.prepare(
        "SELECT schema_name, relation_name, columns
         FROM _tables
         WHERE kind = 'table'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut known = BTreeSet::new();
    let mut declared_xmin = BTreeSet::new();
    for row in rows {
        let (schema, name, columns) = row?;
        let relation = RelationIdentity::new(schema, name);
        let aliases = relation.canonical_and_legacy_public_names();
        let columns = columns.unwrap_or_else(|| "[]".into());
        let definitions = serde_json::from_str::<Vec<serde_json::Value>>(&columns)?;
        let has_declared_xmin = definitions.iter().any(|definition| {
            definition
                .as_object()
                .and_then(|definition| definition.get("name"))
                .and_then(serde_json::Value::as_str)
                == Some("xmin")
        });
        known.extend(aliases.iter().cloned());
        if has_declared_xmin {
            declared_xmin.extend(aliases);
        }
    }
    Ok((known, declared_xmin))
}
