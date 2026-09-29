//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the failing row that NOT NULL, CHECK, partition constraint and view check option violations report in DETAIL: every column for a role with table SELECT, and otherwise the columns the role may select or the statement supplied, listed by name.

use uqa_engine::Engine;

fn verify_failing_rows(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/failing_row_detail_oracle.expected.json"),
    );
}

#[test]
fn failing_row_details_match_postgresql_memory() {
    verify_failing_rows(&Engine::new());
}

#[test]
fn failing_row_details_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_failing_rows(&Engine::open(&directory.path().join("failing-rows.db")).unwrap());
}
