//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional cache invalidation without decoding unchanged catalog data.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "
    CREATE TABLE IF NOT EXISTS _cache_revisions (
        kind TEXT NOT NULL,
        name TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK (typeof(generation) = 'integer' AND generation > 0),
        PRIMARY KEY (kind, name)
    ) WITHOUT ROWID;
",
    )?;
    Catalog::install_cache_revision_tracking(tx)
}
