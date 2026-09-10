//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema catalog and default public schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _schemas (
        name TEXT PRIMARY KEY
    );
    INSERT OR IGNORE INTO _schemas (name) VALUES ('public');
    ";
