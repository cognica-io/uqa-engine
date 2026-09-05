//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent catalog lifecycle helpers for SQLite-backed metadata.

use rusqlite::{params, Connection};

use crate::sqlite::connection::Result;

pub(super) fn columns_json_references(columns_json: &str, column_name: &str) -> Result<bool> {
    Ok(crate::catalog_index_keys::references_column(
        columns_json,
        column_name,
    )?)
}

pub(super) fn renamed_columns_json(
    columns_json: &str,
    from: &str,
    to: &str,
) -> Result<Option<String>> {
    Ok(crate::catalog_index_keys::rename_column(
        columns_json,
        from,
        to,
    )?)
}

pub(super) fn drop_fts_aux_tables_for_table(conn: &Connection, table_name: &str) -> Result<()> {
    let prefixes = [
        format!("_skip_{table_name}_"),
        format!("_blockmax_{table_name}_"),
    ];
    let mut stale_tables = Vec::new();
    for prefix in prefixes {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name LIKE ?1",
        )?;
        let rows = stmt.query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))?;
        for row in rows {
            stale_tables.push(row?);
        }
    }
    for table in stale_tables {
        conn.execute(
            &format!("DROP TABLE IF EXISTS {}", quote_sql_identifier(&table)),
            [],
        )?;
    }
    Ok(())
}

pub(super) fn drop_fts_aux_tables_for_field(
    conn: &Connection,
    table_name: &str,
    field: &str,
) -> Result<()> {
    for table in [
        format!("_skip_{table_name}_{field}"),
        format!("_blockmax_{table_name}_{field}"),
    ] {
        conn.execute(
            &format!("DROP TABLE IF EXISTS {}", quote_sql_identifier(&table)),
            [],
        )?;
    }
    Ok(())
}

pub(super) fn rename_fts_aux_tables_for_field(
    conn: &Connection,
    table_name: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    rename_or_drop_existing_aux_table(
        conn,
        &format!("_skip_{table_name}_{from}"),
        &format!("_skip_{table_name}_{to}"),
    )?;
    rename_or_drop_existing_aux_table(
        conn,
        &format!("_blockmax_{table_name}_{from}"),
        &format!("_blockmax_{table_name}_{to}"),
    )
}

pub(super) fn rename_field_rows_or_keep_existing(
    conn: &Connection,
    table: &str,
    field_column: &str,
    table_name: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    let table = quote_sql_identifier(table);
    let field_column = quote_sql_identifier(field_column);
    conn.execute(
        &format!(
            "UPDATE OR IGNORE {table}
                SET {field_column} = ?3
              WHERE table_name = ?1 AND {field_column} = ?2"
        ),
        params![table_name, from, to],
    )?;
    conn.execute(
        &format!("DELETE FROM {table} WHERE table_name = ?1 AND {field_column} = ?2"),
        params![table_name, from],
    )?;
    Ok(())
}

/// Rename one logical B-tree field without relying on `SQLite`'s foreign-key
/// cascade being enabled by the caller. Updating the parent first satisfies
/// the child key; with `ON UPDATE CASCADE` enabled the second update is simply
/// a no-op.
pub(super) fn rename_btree_field_rows_or_keep_existing(
    conn: &Connection,
    table_name: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    rename_field_rows_or_keep_existing(conn, "_btree_indexes", "field", table_name, from, to)?;
    rename_field_rows_or_keep_existing(conn, "_btree_index_entries", "field", table_name, from, to)
}

pub(super) fn delete_table_rows_if_exists(
    conn: &Connection,
    table: &str,
    table_name: &str,
) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE table_name = ?1",
            quote_sql_identifier(table)
        ),
        params![table_name],
    )?;
    Ok(())
}

pub(super) fn update_table_name_rows_if_exists(
    conn: &Connection,
    table: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    conn.execute(
        &format!(
            "UPDATE {} SET table_name = ?2 WHERE table_name = ?1",
            quote_sql_identifier(table)
        ),
        params![from, to],
    )?;
    Ok(())
}

/// Move a logical B-tree and all of its entries to a new table key without
/// depending on `PRAGMA foreign_keys`. Parent-first ordering gives the child a
/// valid target key; when the declared cascade is active, the child update has
/// no remaining source rows.
pub(super) fn update_btree_table_name_rows_if_exists(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<()> {
    update_table_name_rows_if_exists(conn, "_btree_indexes", from, to)?;
    update_table_name_rows_if_exists(conn, "_btree_index_entries", from, to)
}

pub(super) fn table_exists(conn: &Connection, table_name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table_name],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count != 0)?)
}

fn rename_or_drop_existing_aux_table(
    conn: &Connection,
    from_table: &str,
    to_table: &str,
) -> Result<()> {
    if !table_exists(conn, from_table)? {
        return Ok(());
    }
    if table_exists(conn, to_table)? {
        conn.execute(
            &format!("DROP TABLE IF EXISTS {}", quote_sql_identifier(from_table)),
            [],
        )?;
    } else {
        conn.execute(
            &format!(
                "ALTER TABLE {} RENAME TO {}",
                quote_sql_identifier(from_table),
                quote_sql_identifier(to_table)
            ),
            [],
        )?;
    }
    Ok(())
}

pub(super) fn quote_sql_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
