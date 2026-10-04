//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `select_common_type` and `transformAExprIn`: `oid`, the OID alias types and integers of the numeric category meet at the type the category's preferred type and implicit casts select in `VALUES`, `CASE` (its ELSE result first), `COALESCE`, `UNION`, `ARRAY` and `GREATEST`, `time` meets `timetz`, and an `IN` list in a query, insert, update or delete compares at the common type when one exists and otherwise item by item through `=`.

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
