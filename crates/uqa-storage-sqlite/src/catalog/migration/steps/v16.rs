//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted table-constraint metadata.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let constraints_already_present = Catalog::table_columns(tx, "_tables")?
        .is_some_and(|columns| columns.contains_key("constraints"));
    if !constraints_already_present {
        tx.execute_batch("ALTER TABLE _tables ADD COLUMN constraints TEXT NOT NULL DEFAULT '';")?;
    }
    Ok(())
}
