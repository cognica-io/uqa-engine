//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `MergeAttributes`: a parent named twice and a column declared twice are rejected, each merged column is reported with its notice, type conflicts name both types, a child keeps, overrides or conflicts with its parent's generation as `MergeChildAttribute` decides, parents that give a column different defaults or generation expressions must be overridden, and a temporary relation stays out of a permanent hierarchy.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_merge_attributes(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/merge_attributes_oracle.expected.json"),
    );
}

#[test]
fn merge_attributes_match_postgresql_memory() {
    verify_merge_attributes(&Engine::new());
}

#[test]
fn merge_attributes_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_merge_attributes(&Engine::open(&directory.path().join("merge-attributes.db")).unwrap());
}
