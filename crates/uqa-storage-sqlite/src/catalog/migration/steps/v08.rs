//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted per-column optimizer statistics.

pub(super) const SQL: &str = r"
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
    ";
