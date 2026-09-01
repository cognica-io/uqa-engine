//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent HNSW graph schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _hnsw_indexes (
        table_name        TEXT NOT NULL,
        field             TEXT NOT NULL,
        dimensions        INTEGER NOT NULL,
        m                 INTEGER NOT NULL,
        ef_construction   INTEGER NOT NULL,
        ef_search         INTEGER NOT NULL,
        rebuild_threshold INTEGER NOT NULL,
        seed              TEXT NOT NULL,
        entry_node_id     INTEGER,
        max_level         INTEGER NOT NULL,
        next_node_id      INTEGER NOT NULL,
        live_count        INTEGER NOT NULL,
        deleted_count     INTEGER NOT NULL,
        revision          INTEGER NOT NULL,
        format_version    INTEGER NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _hnsw_nodes (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        node_id        INTEGER NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL,
        level          INTEGER NOT NULL,
        deleted        INTEGER NOT NULL CHECK (deleted IN (0, 1)),
        vector         BLOB NOT NULL,
        PRIMARY KEY (table_name, field, node_id)
    );
    CREATE INDEX IF NOT EXISTS _hnsw_nodes_document_idx
        ON _hnsw_nodes (table_name, field, doc_id, vector_ordinal, deleted);

    CREATE TABLE IF NOT EXISTS _hnsw_edges (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        source_node_id INTEGER NOT NULL,
        layer          INTEGER NOT NULL,
        target_node_id INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, source_node_id, layer, target_node_id)
    );
    CREATE INDEX IF NOT EXISTS _hnsw_edges_source_idx
        ON _hnsw_edges (table_name, field, source_node_id, layer, target_node_id);
    ";
