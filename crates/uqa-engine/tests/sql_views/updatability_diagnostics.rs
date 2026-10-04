//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the errors of DML through views that cannot be rewritten onto their base relation: the message, the DETAIL that `view_query_is_auto_updatable` or `view_col_is_auto_updatable` gives and the HINT of `error_view_not_updatable`, conditional INSTEAD rules before the view's query, columns in view order, rewrite errors at any layer before privilege errors, the action a MERGE reports, and the hint of a rejected check option.

use uqa_engine::Engine;

fn verify_view_updatability(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/view_updatability_oracle.expected.json"),
    );
}

#[test]
fn view_updatability_diagnostics_match_postgresql_memory() {
    verify_view_updatability(&Engine::new());
}

#[test]
fn view_updatability_diagnostics_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_view_updatability(
        &Engine::open(&directory.path().join("view-updatability.db")).unwrap(),
    );
}
