//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog version 20 repair for historical HNSW aliases backed by IVF.

use super::super::super::{params, table_exists, Catalog, RelationIdentity, Result};

struct Candidate {
    relation: Option<RelationIdentity>,
    name: String,
    table_name: String,
    columns_json: String,
}

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
    let shape = Catalog::table_columns(tx, "_catalog_indexes")?.unwrap_or_default();
    let candidates = if shape.contains_key("schema_name") {
        let mut statement = tx.prepare(
            "SELECT schema_name, relation_name, table_schema_name,
                    table_relation_name, columns
               FROM _catalog_indexes
              WHERE lower(index_type) = 'hnsw'
              ORDER BY schema_name, relation_name",
        )?;
        let rows = statement.query_map([], |row| {
            let schema = row.get::<_, String>(0)?;
            let name = row.get::<_, String>(1)?;
            let table_schema = row.get::<_, String>(2)?;
            let table_name = row.get::<_, String>(3)?;
            Ok(Candidate {
                relation: Some(RelationIdentity::new(schema, &name)),
                name,
                table_name: RelationIdentity::new(table_schema, table_name).qualified_name(),
                columns_json: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut statement = tx.prepare(
            "SELECT name, table_name, columns
               FROM _catalog_indexes
              WHERE lower(index_type) = 'hnsw'
              ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Candidate {
                relation: None,
                name: row.get(0)?,
                table_name: row.get(1)?,
                columns_json: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut legacy_aliases = Vec::new();
    for candidate in candidates {
        let columns: Vec<String> = serde_json::from_str(&candidate.columns_json)?;
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
                params![candidate.table_name, field],
                |row| row.get::<_, bool>(0),
            )?;
            let has_hnsw = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM _hnsw_indexes
                      WHERE table_name = ?1 AND field = ?2
                 )",
                params![candidate.table_name, field],
                |row| row.get::<_, bool>(0),
            )?;
            all_ivf &= has_ivf;
            any_hnsw |= has_hnsw;
        }
        if all_ivf && !any_hnsw {
            legacy_aliases.push(candidate);
        }
    }

    for candidate in legacy_aliases {
        if let Some(relation) = candidate.relation {
            tx.execute(
                "UPDATE _catalog_indexes
                    SET index_type = 'ivf'
                  WHERE schema_name = ?1 AND relation_name = ?2
                    AND lower(index_type) = 'hnsw'",
                params![relation.schema, relation.name],
            )?;
        } else {
            tx.execute(
                "UPDATE _catalog_indexes
                    SET index_type = 'ivf'
                  WHERE name = ?1 AND lower(index_type) = 'hnsw'",
                params![candidate.name],
            )?;
        }
    }
    Ok(())
}
