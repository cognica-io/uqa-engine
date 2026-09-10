//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist the physical sequence log counter.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_sequences")?.unwrap_or_default();
    if !columns.contains_key("log_count") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN log_count INTEGER NOT NULL DEFAULT 0 CHECK (log_count >= 0);",
        )?;
    }
    Ok(())
}
