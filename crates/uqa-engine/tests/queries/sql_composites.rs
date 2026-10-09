//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for standalone composite types: `CREATE TYPE ... AS (...)` in `DefineCompositeType`'s check order, the type, array type and composite relation in `pg_type`, `pg_class`, `pg_attribute` and `pg_depend`, `record_in` and `record_out`, row coercion, field selection and assignment, record comparison and null tests, arrays and nesting, relation commands that refuse a composite relation, view definitions, dependency-aware drops that remove attributes, and the type lifecycle.

use uqa_engine::Engine;

#[path = "sql_composites/addition.rs"]
mod addition;

#[path = "sql_composites/range_restoration.rs"]
mod range_restoration;
#[path = "sql_composites/removal.rs"]
mod removal;
#[path = "sql_composites/renaming.rs"]
mod renaming;
#[path = "sql_composites/schema_range_restoration.rs"]
mod schema_range_restoration;
#[path = "sql_composites/type_changes.rs"]
mod type_changes;

fn verify_composite_types(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/composite_type_oracle.expected.json"),
    );
}

#[test]
fn composite_types_match_postgresql_memory() {
    verify_composite_types(&Engine::new());
}

#[test]
fn composite_types_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_composite_types(&Engine::open(&directory.path().join("composites.db")).unwrap());
}
