//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model catalog schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _models (
        name TEXT PRIMARY KEY,
        body TEXT NOT NULL
    );
    ";
