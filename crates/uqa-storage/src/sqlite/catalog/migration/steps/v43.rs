//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable secondary-index semantic definitions.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_catalog_indexes")?.unwrap_or_default();
    if !columns.contains_key("definition") {
        tx.execute_batch("ALTER TABLE _catalog_indexes ADD COLUMN definition TEXT")?;
    }
    Ok(())
}
