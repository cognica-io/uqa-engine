//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scope graph invalidation to its durable owners, not the SQL registries.

use super::super::super::{quote_sql_identifier, Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // Some relational-only legacy catalogs never installed graph storage.
    // Install missing tables without replacing any existing graph contents.
    tx.execute_batch(super::v06::CREATE_SQL)?;
    // Replace only triggers owned by the catalog cache protocol. Keep the
    // counters: a dropped/recreated graph must never reuse an old revision.
    for table in [
        "_named_graphs",
        "_graph_vertices",
        "_graph_edges",
        "_graph_membership",
        "_metadata",
    ] {
        for event in ["INSERT", "DELETE", "UPDATE"] {
            let name = quote_sql_identifier(&format!("uqa_cache_{table}_{event}"));
            tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
        }
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS _graph_membership_by_graph \
             ON _graph_membership(graph_name, entity_type, entity_id); \
         INSERT INTO _cache_revisions(kind, name, generation) \
             SELECT 'graph', name, 1 FROM ( \
                 SELECT name FROM _named_graphs \
                 UNION SELECT graph_name AS name FROM _graph_membership \
             ) WHERE true \
             ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1;",
    )?;
    Catalog::install_cache_revision_tracking(tx)
}
