//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF vector-index metadata, centroids, and assignments.

pub(super) const SQL: &str = r"
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
    ";
