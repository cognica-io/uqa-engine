//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Removal of the redundant posting-term index.

pub(super) const SQL: &str = r"
    DROP INDEX IF EXISTS _postings_term_idx;
    ";
