//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain exact named descriptors and independent field analyzer bindings.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Connection) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS _analyzers (name TEXT PRIMARY KEY, config_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS _table_field_analyzers (table_name TEXT NOT NULL, field TEXT NOT NULL, phase TEXT NOT NULL, analyzer_name TEXT NOT NULL, PRIMARY KEY(table_name, field, phase));")?;
    for (table, column) in [
        ("_analyzers", "descriptor_json"),
        ("_table_field_analyzers", "binding_json"),
    ] {
        let mut statement = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !columns.iter().any(|name| name == column) {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"))?;
        }
    }
    Catalog::install_cache_revision_tracking(tx)
}
