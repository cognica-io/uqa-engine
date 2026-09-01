//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial sequence catalog schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _sequences (
        name      TEXT PRIMARY KEY,
        start     INTEGER NOT NULL,
        increment INTEGER NOT NULL,
        current   INTEGER NOT NULL
    );
    ";
