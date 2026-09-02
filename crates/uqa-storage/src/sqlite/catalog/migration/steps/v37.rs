//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist ordinary-table column access-control lists.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_tables")?.unwrap_or_default();
    if !columns.contains_key("column_acls_json") {
        tx.execute_batch("ALTER TABLE _tables ADD COLUMN column_acls_json TEXT;")?;
    }
    Ok(())
}
