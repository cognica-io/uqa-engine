//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist SQL role ownership for sequences.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_sequences")?.unwrap_or_default();
    if !columns.contains_key("role_owner") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN role_owner TEXT NOT NULL DEFAULT 'uqa';",
        )?;
    }
    Ok(())
}
