//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent sequence object identities.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let object_id_already_present = Catalog::table_columns(tx, "_sequences")?
        .is_some_and(|columns| columns.contains_key("object_id"));
    if !object_id_already_present {
        tx.execute_batch(
            "ALTER TABLE _sequences
                ADD COLUMN object_id BLOB NOT NULL DEFAULT X'00000000000000000000000000000000';",
        )?;
    }
    Ok(())
}
