//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered `SQLite` schema migrations.

/// Migrations applied in order. Each version and SQL pair is run in a single
/// transaction; the metadata schema-version row is bumped on success.
pub(super) const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r"
    CREATE TABLE IF NOT EXISTS _tables (
        name           TEXT PRIMARY KEY,
        analyzer       TEXT NOT NULL,
        fts_fields     TEXT NOT NULL,
        vector_fields  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _documents (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        body       TEXT NOT NULL,
        PRIMARY KEY (table_name, doc_id)
    );

    CREATE TABLE IF NOT EXISTS _postings (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        term       TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        positions  BLOB NOT NULL,
        PRIMARY KEY (table_name, field, term, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _postings_doc_idx
        ON _postings (table_name, doc_id);

    CREATE TABLE IF NOT EXISTS _doc_lengths (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        field      TEXT NOT NULL,
        length     INTEGER NOT NULL,
        PRIMARY KEY (table_name, doc_id, field)
    );

    CREATE TABLE IF NOT EXISTS _field_stats (
        table_name   TEXT NOT NULL,
        field        TEXT NOT NULL,
        total_length INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _vectors (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        vector     BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    ",
    ),
    (
        2,
        r"
    CREATE TABLE IF NOT EXISTS _models (
        name TEXT PRIMARY KEY,
        body TEXT NOT NULL
    );
    ",
    ),
    (
        3,
        r"
    ALTER TABLE _tables ADD COLUMN columns TEXT;
    ",
    ),
    (
        4,
        r"
    CREATE TABLE IF NOT EXISTS _scoring_params (
        name TEXT PRIMARY KEY,
        params TEXT NOT NULL
    );
    ",
    ),
    (
        5,
        r"
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
    ",
    ),
    // Re-shape graph storage into normalized global graph tables:
    // global vertex / edge tables keyed by id, a separate
    // `_graph_membership` table mapping each entity to one or more
    // named graphs, and the four supporting indexes the planner needs
    // for label-based lookups. The legacy v5 tables (denormalized by
    // graph name + JSON body) get dropped because no engine call site
    // reads them anymore.
    (
        6,
        r"
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
    ",
    ),
    // Persist the five engine-side registries that previously lived
    // only in `Engine`'s in-memory maps (named analyzers, table-field
    // analyzer overrides, foreign servers / tables, registered
    // indexes, graph path indexes) with durable table and column shapes.
    (
        7,
        r"
    CREATE TABLE IF NOT EXISTS _analyzers (
        name        TEXT PRIMARY KEY,
        config_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _table_field_analyzers (
        table_name    TEXT NOT NULL,
        field         TEXT NOT NULL,
        phase         TEXT NOT NULL,
        analyzer_name TEXT NOT NULL,
        PRIMARY KEY (table_name, field, phase)
    );

    CREATE TABLE IF NOT EXISTS _foreign_servers (
        name     TEXT PRIMARY KEY,
        fdw_type TEXT NOT NULL,
        options  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _foreign_tables (
        name         TEXT PRIMARY KEY,
        server_name  TEXT NOT NULL,
        columns_json TEXT NOT NULL,
        options      TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _catalog_indexes (
        name       TEXT PRIMARY KEY,
        index_type TEXT NOT NULL,
        table_name TEXT NOT NULL,
        columns    TEXT NOT NULL,
        parameters TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _path_indexes (
        graph_name      TEXT PRIMARY KEY,
        label_sequences TEXT NOT NULL
    );
    ",
    ),
    // Persist per-column statistics produced by ANALYZE so that the
    // optimiser still has cardinality and range estimates after a restart.
    (
        8,
        r"
    CREATE TABLE IF NOT EXISTS _column_stats (
        table_name      TEXT NOT NULL,
        column_name     TEXT NOT NULL,
        distinct_count  INTEGER NOT NULL,
        null_count      INTEGER NOT NULL,
        min_value       TEXT,
        max_value       TEXT,
        row_count       INTEGER NOT NULL,
        histogram       TEXT NOT NULL DEFAULT '[]',
        mcv_values      TEXT NOT NULL DEFAULT '[]',
        mcv_frequencies TEXT NOT NULL DEFAULT '[]',
        PRIMARY KEY (table_name, column_name)
    );
    ",
    ),
    (
        9,
        r"
    CREATE TABLE IF NOT EXISTS _ivf_indexes (
        table_name          TEXT NOT NULL,
        field               TEXT NOT NULL,
        dimensions          INTEGER NOT NULL,
        nlist               INTEGER NOT NULL,
        nprobe              INTEGER NOT NULL,
        train_threshold     INTEGER NOT NULL,
        state               TEXT NOT NULL,
        trained_size        INTEGER NOT NULL,
        deletes_since_train INTEGER NOT NULL,
        vector_count        INTEGER NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _ivf_centroids (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        centroid_id INTEGER NOT NULL,
        vector      BLOB NOT NULL,
        PRIMARY KEY (table_name, field, centroid_id)
    );

    CREATE TABLE IF NOT EXISTS _ivf_assignments (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        doc_id      INTEGER NOT NULL,
        centroid_id INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id);
    ",
    ),
    (
        10,
        r"
    CREATE TABLE IF NOT EXISTS _vectors_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        vector         BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _vectors_v10
        (table_name, field, doc_id, vector_ordinal, vector)
        SELECT table_name, field, doc_id, 0, vector FROM _vectors;
    DROP TABLE IF EXISTS _vectors;
    ALTER TABLE _vectors_v10 RENAME TO _vectors;

    CREATE TABLE IF NOT EXISTS _ivf_assignments_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        centroid_id    INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _ivf_assignments_v10
        (table_name, field, doc_id, vector_ordinal, centroid_id)
        SELECT table_name, field, doc_id, 0, centroid_id FROM _ivf_assignments;
    DROP TABLE IF EXISTS _ivf_assignments;
    ALTER TABLE _ivf_assignments_v10 RENAME TO _ivf_assignments;
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id, vector_ordinal);
    ",
    ),
    // Map logical btree indexes to compact durable postings. The engine
    // hydrates its in-memory B-tree from these rows on reopen instead of
    // reparsing every full document on the first indexed predicate.
    (
        11,
        r"
    CREATE TABLE IF NOT EXISTS _btree_indexes (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _btree_index_entries (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        value_json TEXT NOT NULL,
        PRIMARY KEY (table_name, field, doc_id),
        FOREIGN KEY (table_name, field)
            REFERENCES _btree_indexes (table_name, field)
            ON UPDATE CASCADE ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS _btree_index_value_idx
        ON _btree_index_entries (table_name, field, value_json, doc_id);
    CREATE INDEX IF NOT EXISTS _btree_index_doc_idx
        ON _btree_index_entries (table_name, doc_id);
    ",
    ),
    // `_postings` already has a unique auto-index over
    // `(table_name, field, term, doc_id)`. Its first three columns cover
    // term lookup, so the former `_postings_term_idx` duplicated every FTS
    // write without enabling a distinct access path.
    (
        12,
        r"
    DROP INDEX IF EXISTS _postings_term_idx;
    ",
    ),
    (
        13,
        r"
    CREATE TABLE IF NOT EXISTS _schemas (
        name TEXT PRIMARY KEY
    );
    INSERT OR IGNORE INTO _schemas (name) VALUES ('public');
    ",
    ),
    (
        14,
        r"
    CREATE TABLE IF NOT EXISTS _sequences (
        name      TEXT PRIMARY KEY,
        start     INTEGER NOT NULL,
        increment INTEGER NOT NULL,
        current   INTEGER NOT NULL
    );
    ",
    ),
    // Keep large binary and numeric document values out of JSON bodies. This
    // table used to be created lazily by document reads and writes, which
    // turned a read into schema-changing DDL and serialized WAL readers behind
    // an active writer. Catalog migration now guarantees its existence before
    // any document store is exposed.
    (
        15,
        r"
    CREATE TABLE IF NOT EXISTS _document_blobs (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        field_name TEXT NOT NULL,
        bytes      BLOB NOT NULL,
        PRIMARY KEY (table_name, doc_id, field_name)
    );
    ",
    ),
    // Persist table CHECK / FOREIGN KEY / composite PRIMARY KEY and UNIQUE
    // metadata. Existing catalogs receive an empty payload which the engine
    // interprets as the pre-v16 default constraint set.
    (
        16,
        r"
    ALTER TABLE _tables ADD COLUMN constraints TEXT NOT NULL DEFAULT '';
    ",
    ),
    // Replace flat relation-name strings with a shared schema-owned relation
    // catalog. The data rewrite and collision preflight are implemented in
    // Rust so legacy view/sequence JSON can migrate in the same transaction.
    (17, ""),
    // A sequence needs an explicit first-allocation bit.  The former
    // `current = start - increment` sentinel cannot represent valid BIGINT
    // boundary starts. Existing rows use the old sentinel representation, so
    // `called = 1` preserves their next-value behavior exactly.
    (
        18,
        r"
    ALTER TABLE _sequences
        ADD COLUMN called INTEGER NOT NULL DEFAULT 1 CHECK (called IN (0, 1));
    ",
    ),
    (
        19,
        r"
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
    ",
    ),
    // Before persistent HNSW existed, `CREATE INDEX ... USING hnsw` was an
    // alias for the SQLite IVF implementation. Some historical catalogs kept
    // the requested `hnsw` spelling even though their physical metadata lives
    // in `_ivf_indexes`. The data-dependent rewrite is implemented in Rust so
    // it can parse the catalog's JSON column list without requiring SQLite's
    // optional JSON extension.
    (20, ""),
];
