//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog version 23 sequence-persistence migration.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let persistence_already_present = Catalog::table_columns(tx, "_sequences")?
        .is_some_and(|columns| columns.contains_key("persistence"));
    if !persistence_already_present {
        tx.execute_batch(
            "ALTER TABLE _sequences
                ADD COLUMN persistence TEXT NOT NULL DEFAULT 'p'
                CHECK (persistence IN ('p', 'u'));",
        )?;
    }
    Ok(tx.execute_batch(
        "UPDATE _sequences
            SET persistence = COALESCE(
                (SELECT NULLIF(value, '')
                   FROM _metadata
                  WHERE key = 'sequence-persistence:' || _sequences.schema_name || '.' || _sequences.relation_name),
                'p'
            );
         DELETE FROM _metadata WHERE key LIKE 'sequence-persistence:%';",
    )?)
}
