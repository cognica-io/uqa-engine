//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Store path reachability in physical indexes and invalidate it with graph writes.
use super::super::super::{Catalog, Result};
use std::fmt::Write as _;

pub(super) fn migrate(tx: &rusqlite::Connection) -> Result<()> {
    tx.execute_batch(
        // Relational-only legacy catalogs may not have installed path
        // definitions. Install the optional base table without replacing
        // any existing definitions, before adding dependent triggers.
        "CREATE TABLE IF NOT EXISTS _path_indexes (
            graph_name TEXT PRIMARY KEY,
            label_sequences TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS _graph_path_index_state (
            index_key TEXT PRIMARY KEY,
            graph_name TEXT NOT NULL,
            definition_json TEXT NOT NULL,
            valid INTEGER NOT NULL CHECK(valid IN (0, 1))
        );
        CREATE INDEX IF NOT EXISTS _graph_path_index_by_graph ON _graph_path_index_state(graph_name);
        CREATE TABLE IF NOT EXISTS _graph_path_pairs (
            index_key TEXT NOT NULL,
            sequence_key TEXT NOT NULL,
            source_id INTEGER NOT NULL CHECK(source_id >= 0),
            target_id INTEGER NOT NULL CHECK(target_id >= 0),
            PRIMARY KEY(index_key, sequence_key, source_id, target_id)
        ) WITHOUT ROWID;
        CREATE TRIGGER IF NOT EXISTS uqa_path_definition_delete AFTER DELETE ON _path_indexes BEGIN
            DELETE FROM _graph_path_pairs WHERE index_key = OLD.graph_name;
            DELETE FROM _graph_path_index_state WHERE index_key = OLD.graph_name;
        END;
        CREATE TRIGGER IF NOT EXISTS uqa_path_definition_update AFTER UPDATE ON _path_indexes BEGIN
            UPDATE _graph_path_index_state SET valid = 0 WHERE index_key IN (OLD.graph_name, NEW.graph_name);
        END;
        CREATE TRIGGER IF NOT EXISTS uqa_path_definition_insert AFTER INSERT ON _path_indexes BEGIN
            UPDATE _graph_path_index_state SET valid = 0 WHERE index_key = NEW.graph_name;
        END;"
    )?;
    for (table, column, kind) in [
        ("_graph_vertices", "vertex_id", "vertex"),
        ("_graph_edges", "edge_id", "edge"),
        ("_graph_membership", "graph_name", ""),
        ("_named_graphs", "name", ""),
        ("_metadata", "key", ""),
    ] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let images: &[&str] = match event {
                "INSERT" => &["NEW"],
                "DELETE" => &["OLD"],
                _ => &["OLD", "NEW"],
            };
            let mut body = String::new();
            for image in images {
                let graph_names = if !kind.is_empty() {
                    format!("SELECT graph_name FROM _graph_membership WHERE entity_type = '{kind}' AND entity_id = {image}.{column}")
                } else if table == "_metadata" {
                    format!("SELECT substr({image}.key, 23) WHERE substr({image}.key, 1, 22) = 'graph_label_registry::'")
                } else {
                    format!("SELECT {image}.{column}")
                };
                write!(body, "UPDATE _graph_path_index_state SET valid = 0 WHERE graph_name IN ({graph_names});").expect("write path validity trigger");
            }
            tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS uqa_path_{table}_{event} AFTER {event} ON {table} BEGIN {body} END;"))?;
        }
    }
    Catalog::install_cache_revision_tracking(tx)
}
