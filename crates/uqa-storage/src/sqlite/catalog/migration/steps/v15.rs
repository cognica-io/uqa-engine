//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Separate binary and large numeric values from document JSON bodies.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _document_blobs (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        field_name TEXT NOT NULL,
        bytes      BLOB NOT NULL,
        PRIMARY KEY (table_name, doc_id, field_name)
    );
    ";
