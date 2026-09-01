//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-vector ordinals for vector storage and IVF assignments.

pub(super) const SQL: &str = r"
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
    ";
