//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalized global graph tables and lookup indexes.

pub(super) const SQL: &str = r"
    DROP TABLE IF EXISTS _graphs;
    DROP TABLE IF EXISTS _graph_vertices;
    DROP TABLE IF EXISTS _graph_edges;

    CREATE TABLE IF NOT EXISTS _named_graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        vertex_id       INTEGER PRIMARY KEY,
        label           TEXT NOT NULL DEFAULT '',
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        edge_id         INTEGER PRIMARY KEY,
        source_id       INTEGER NOT NULL,
        target_id       INTEGER NOT NULL,
        label           TEXT NOT NULL,
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_membership (
        entity_type TEXT NOT NULL,
        entity_id   INTEGER NOT NULL,
        graph_name  TEXT NOT NULL,
        PRIMARY KEY (entity_type, entity_id, graph_name)
    );

    CREATE INDEX IF NOT EXISTS _graph_vertices_label
        ON _graph_vertices (label);
    CREATE INDEX IF NOT EXISTS _graph_edges_out
        ON _graph_edges (source_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_in
        ON _graph_edges (target_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_label
        ON _graph_edges (label);
    ";
