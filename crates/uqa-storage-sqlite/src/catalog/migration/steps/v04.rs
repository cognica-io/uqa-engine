//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoring-parameter catalog schema.

pub(super) const SQL: &str = r"
    CREATE TABLE IF NOT EXISTS _scoring_params (
        name TEXT PRIMARY KEY,
        params TEXT NOT NULL
    );
    ";
