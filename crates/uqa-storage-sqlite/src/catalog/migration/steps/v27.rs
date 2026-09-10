//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist declared sequence types, bounds, and cycling behavior.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_sequences")?.unwrap_or_default();
    if !columns.contains_key("data_type") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN data_type TEXT NOT NULL DEFAULT 'bigint';",
        )?;
    }
    if !columns.contains_key("min_value") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN min_value INTEGER NOT NULL DEFAULT 0;
             UPDATE _sequences
                SET min_value = CASE WHEN increment > 0 THEN 1 ELSE (-9223372036854775807 - 1) END;",
        )?;
    }
    if !columns.contains_key("max_value") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN max_value INTEGER NOT NULL DEFAULT 0;
             UPDATE _sequences
                SET max_value = CASE WHEN increment > 0 THEN 9223372036854775807 ELSE -1 END;",
        )?;
    }
    if !columns.contains_key("cycle") {
        tx.execute_batch(
            "ALTER TABLE _sequences ADD COLUMN cycle INTEGER NOT NULL DEFAULT 0 CHECK (cycle IN (0, 1));",
        )?;
    }
    Ok(())
}
