//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable scalar B-tree markers and postings.

pub(super) const SQL: &str = r"
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
    ";
