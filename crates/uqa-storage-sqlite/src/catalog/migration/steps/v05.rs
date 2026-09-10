//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial graph catalog schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        graph     TEXT NOT NULL,
        vertex_id INTEGER NOT NULL,
        body      TEXT NOT NULL,
        PRIMARY KEY (graph, vertex_id)
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        graph   TEXT NOT NULL,
        edge_id INTEGER NOT NULL,
        body    TEXT NOT NULL,
        PRIMARY KEY (graph, edge_id)
    );
    CREATE INDEX IF NOT EXISTS _graph_edges_by_graph
        ON _graph_edges (graph);
    ";
