//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial table, document, posting, statistics, and vector schema.

pub(super) const SQL: &str = r"
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
    ";
