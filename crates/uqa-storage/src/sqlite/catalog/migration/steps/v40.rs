//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist foreign-table role ownership.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_foreign_tables")?.unwrap_or_default();
    if !columns.contains_key("role_owner") {
        tx.execute_batch(
            "ALTER TABLE _foreign_tables
             ADD COLUMN role_owner TEXT NOT NULL DEFAULT 'uqa';",
        )?;
    }
    Ok(())
}
