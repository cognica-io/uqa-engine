//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog version 20 repair for historical HNSW aliases backed by IVF.

use super::super::super::{params, table_exists, Result};

/// Correct historical `hnsw` catalog rows whose durable implementation is IVF. HNSW used to be a SQL alias for IVF, and a few releases persisted the requested spelling rather than the physical index kind. Treating those rows as native HNSW after v19 makes engine reopen fail because no `_hnsw_indexes` row can exist for them.
///
/// Physical metadata is the source of truth: rewrite only when every indexed column has IVF metadata and none has HNSW metadata. Genuine persistent HNSW indexes are therefore left untouched.
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "_catalog_indexes")?
        || !table_exists(tx, "_ivf_indexes")?
        || !table_exists(tx, "_hnsw_indexes")?
    {
        return Ok(());
    }
    let candidates = {
        let mut statement = tx.prepare(
            "SELECT name, table_name, columns
               FROM _catalog_indexes
              WHERE lower(index_type) = 'hnsw'
              ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut legacy_aliases = Vec::new();
    for (name, table_name, columns_json) in candidates {
        let columns: Vec<String> = serde_json::from_str(&columns_json)?;
        if columns.is_empty() {
            continue;
        }
        let mut all_ivf = true;
        let mut any_hnsw = false;
        for field in columns {
            let has_ivf = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM _ivf_indexes
                      WHERE table_name = ?1 AND field = ?2
                 )",
                params![table_name, field],
                |row| row.get::<_, bool>(0),
            )?;
            let has_hnsw = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM _hnsw_indexes
                      WHERE table_name = ?1 AND field = ?2
                 )",
                params![table_name, field],
                |row| row.get::<_, bool>(0),
            )?;
            all_ivf &= has_ivf;
            any_hnsw |= has_hnsw;
        }
        if all_ivf && !any_hnsw {
            legacy_aliases.push(name);
        }
    }

    for name in legacy_aliases {
        tx.execute(
            "UPDATE _catalog_indexes
                SET index_type = 'ivf'
              WHERE name = ?1 AND lower(index_type) = 'hnsw'",
            params![name],
        )?;
    }
    Ok(())
}
