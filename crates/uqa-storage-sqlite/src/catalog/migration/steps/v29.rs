//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist stable sequence-owner dependencies.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_sequences")?.unwrap_or_default();
    if !columns.contains_key("owner_table_object_id") {
        tx.execute_batch("ALTER TABLE _sequences ADD COLUMN owner_table_object_id BLOB;")?;
    }
    if !columns.contains_key("owner_column_object_id") {
        tx.execute_batch("ALTER TABLE _sequences ADD COLUMN owner_column_object_id BLOB;")?;
    }
    if !columns.contains_key("owner_dependency") {
        tx.execute_batch("ALTER TABLE _sequences ADD COLUMN owner_dependency TEXT;")?;
    }
    Ok(())
}
