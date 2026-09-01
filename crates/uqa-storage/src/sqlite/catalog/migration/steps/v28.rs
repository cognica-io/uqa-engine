//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist sequence cache sizes and definition generations.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_sequences")?.unwrap_or_default();
    if !columns.contains_key("cache_size") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN cache_size INTEGER NOT NULL DEFAULT 1 CHECK (cache_size > 0);",
        )?;
    }
    if !columns.contains_key("definition_generation") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN definition_generation BLOB NOT NULL DEFAULT X'';
             UPDATE _sequences SET definition_generation = object_id;",
        )?;
    }
    Ok(())
}
