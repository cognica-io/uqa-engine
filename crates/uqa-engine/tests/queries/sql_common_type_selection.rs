//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `select_common_type`, `coerce_to_common_type` and `transformAExprIn`: `oid`, the OID alias types and integers of the numeric category meet at the type the category's preferred type and implicit casts select in `VALUES`, `CASE` (its ELSE result first), `COALESCE`, `UNION`, `ARRAY` and `GREATEST`, `time` meets `timetz`, and an `IN` list in a query, insert, update or delete compares at the common type when one exists and otherwise item by item through `=`. A conflict names its construct (`UNION types integer and date cannot be matched`, `CASE/WHEN could not convert type regtype to regclass`), inputs that are all `unknown` resolve to `text`, and an `unknown` literal is read by the selected type's input function before the statement runs.

use uqa_engine::Engine;

fn verify_common_type_selection(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/common_type_selection_oracle.expected.json"),
    );
}

#[test]
fn common_type_selection_matches_postgresql_memory() {
    verify_common_type_selection(&Engine::new());
}

#[test]
fn common_type_selection_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_common_type_selection(
        &Engine::open(&directory.path().join("common-type-selection.db")).unwrap(),
    );
}

fn verify_common_type_contexts(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/common_type_contexts_oracle.expected.json"),
    );
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/constant_source_types_oracle.expected.json"),
    );
}

#[test]
fn common_type_contexts_match_postgresql_memory() {
    verify_common_type_contexts(&Engine::new());
}

#[test]
fn common_type_contexts_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_common_type_contexts(
        &Engine::open(&directory.path().join("common-type-contexts.db")).unwrap(),
    );
}
