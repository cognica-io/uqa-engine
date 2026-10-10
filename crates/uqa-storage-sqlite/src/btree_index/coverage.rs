//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native physical population certificates. Posting identity is unique and guarded by document membership, so equal populations certify complete support without enumerating either collection on a read.

use super::Result;
use rusqlite::{params, types::ValueRef, Connection};

pub(crate) const TABLES: [(&str, &str); 2] = [
    ("_btree_document_population", "CREATE TABLE _btree_document_population (table_name TEXT PRIMARY KEY NOT NULL, population INTEGER NOT NULL CHECK(population >= 0)) WITHOUT ROWID"),
    ("_btree_index_population", "CREATE TABLE _btree_index_population (table_name TEXT NOT NULL, field TEXT NOT NULL, population INTEGER NOT NULL CHECK(population >= 0), PRIMARY KEY(table_name, field)) WITHOUT ROWID"),
];

pub(crate) fn triggers() -> Vec<(String, String)> {
    let mut definitions = Vec::new();
    for (source, target, keys) in [
        ("_documents", TABLES[0].0, &["table_name"][..]),
        (
            "_btree_index_entries",
            TABLES[1].0,
            &["table_name", "field"][..],
        ),
    ] {
        let columns = keys.join(", ");
        let values = |row: &str| {
            keys.iter()
                .map(|key| format!("{row}.{key}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let old_where = keys
            .iter()
            .map(|key| format!("{key} = OLD.{key}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let changed = keys
            .iter()
            .map(|key| format!("OLD.{key} IS NOT NEW.{key}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let insert = format!("INSERT INTO {target} ({columns}, population) VALUES ({}, 1) ON CONFLICT ({columns}) DO UPDATE SET population = population + 1;", values("NEW"));
        let delete = format!("UPDATE {target} SET population = population - 1 WHERE {old_where};");
        for (action, event, body) in [
            ("insert", "INSERT".to_owned(), insert.clone()),
            ("delete", "DELETE".to_owned(), delete.clone()),
            (
                "update",
                format!("UPDATE OF {columns}"),
                format!("{delete} {insert}"),
            ),
        ] {
            let name = format!("_btree_population{source}_{action}");
            let when = if action == "update" {
                format!(" WHEN {changed}")
            } else {
                String::new()
            };
            let sql =
                format!("CREATE TRIGGER {name} AFTER {event} ON {source}{when} BEGIN {body} END");
            definitions.push((name, sql));
        }
    }
    definitions
}

/// Only native bootstrap/upgrade calls this, under its owning format transaction. Native publication uses guarded UPSERT/DELETE statements; legacy raw writers never expose this capability.
pub(crate) fn install(connection: &Connection) -> Result<()> {
    for (_, sql) in TABLES {
        connection.execute_batch(&sql.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1))?;
    }
    connection.execute_batch("DELETE FROM _btree_document_population; INSERT INTO _btree_document_population SELECT table_name, count(*) FROM _documents GROUP BY table_name; DELETE FROM _btree_index_population; INSERT INTO _btree_index_population SELECT table_name, field, count(*) FROM _btree_index_entries GROUP BY table_name, field;")?;
    // Predecessor physical writers may have left dangling postings. Keep their existing sparse repair path; equal counts alone cannot certify a set that is not a subset of the documents.
    connection.execute_batch("INSERT OR IGNORE INTO _btree_index_repairs(table_name, field) SELECT entry.table_name, entry.field FROM _btree_index_entries entry WHERE NOT EXISTS(SELECT 1 FROM _documents document WHERE document.table_name = entry.table_name AND document.doc_id = entry.doc_id) GROUP BY entry.table_name, entry.field;")?;
    for (_, sql) in triggers() {
        connection.execute_batch(&sql.replacen(
            "CREATE TRIGGER",
            "CREATE TRIGGER IF NOT EXISTS",
            1,
        ))?;
    }
    Ok(())
}

pub(super) fn complete(connection: &Connection, table: &str, field: ValueRef<'_>) -> Result<bool> {
    let mut statement = connection.prepare_cached("SELECT coalesce((SELECT population FROM _btree_document_population WHERE table_name = ?1), 0) = coalesce((SELECT population FROM _btree_index_population WHERE table_name = ?1 AND field = ?2), 0)")?;
    let complete = statement.query_row(
        params![table, rusqlite::types::ToSqlOutput::Borrowed(field)],
        |row| row.get(0),
    )?;
    #[cfg(test)]
    super::probe::PROBE_VM_STEPS.set(
        super::probe::PROBE_VM_STEPS.get()
            + statement.reset_status(rusqlite::StatementStatus::VmStep) as usize,
    );
    Ok(complete)
}
