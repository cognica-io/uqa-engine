//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit sequence first-allocation state.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let called_already_present = Catalog::table_columns(tx, "_sequences")?
        .is_some_and(|columns| columns.contains_key("called"));
    if !called_already_present {
        tx.execute_batch(
            "ALTER TABLE _sequences
                ADD COLUMN called INTEGER NOT NULL DEFAULT 1 CHECK (called IN (0, 1));",
        )?;
    }
    Ok(())
}
