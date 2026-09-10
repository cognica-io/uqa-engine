//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist foreign-table relation and column ACLs.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_foreign_tables")?.unwrap_or_default();
    if !columns.contains_key("acl_json") {
        tx.execute_batch("ALTER TABLE _foreign_tables ADD COLUMN acl_json TEXT;")?;
    }
    if !columns.contains_key("column_acls_json") {
        tx.execute_batch(
            "ALTER TABLE _foreign_tables
             ADD COLUMN column_acls_json TEXT NOT NULL DEFAULT '{}';",
        )?;
    }
    tx.execute(
        "UPDATE _foreign_tables SET column_acls_json = '{}' WHERE column_acls_json IS NULL",
        [],
    )?;
    Ok(())
}
