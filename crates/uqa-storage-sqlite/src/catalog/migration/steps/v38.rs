//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist regular-view and materialized-view role ownership.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_views")?.unwrap_or_default();
    if !columns.contains_key("role_owner") {
        tx.execute_batch("ALTER TABLE _views ADD COLUMN role_owner TEXT NOT NULL DEFAULT 'uqa';")?;
    }
    Ok(())
}
