//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted table-column metadata.

pub(super) const SQL: &str = r"
    ALTER TABLE _tables ADD COLUMN columns TEXT;
    ";
