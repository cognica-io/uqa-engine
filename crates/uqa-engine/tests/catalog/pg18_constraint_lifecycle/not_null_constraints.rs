//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `AddRelationNotNullConstraints`: `CREATE TABLE` creates its NOT NULL constraints after its CHECK constraints, one per column, from the column clauses, table constraints and PRIMARY KEY columns in declaration order and then from what the parents give; conflicting NO INHERIT declarations and names, unknown and system columns, NO INHERIT on an inherited constraint, repeated names and names a CHECK constraint holds report `PostgreSQL`'s SQLSTATEs, inherited constraints keep their parents' names unless the relation holds them, every constraint is validated, and a failing statement has used the OIDs `PostgreSQL` uses.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn verify_not_null_constraints(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/not_null_constraints_oracle.expected.json"),
    );
}

#[test]
fn not_null_constraints_match_postgresql_memory() {
    verify_not_null_constraints(&Engine::new());
}

#[test]
fn not_null_constraints_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_not_null_constraints(
        &Engine::open(&directory.path().join("not-null-constraints.db")).unwrap(),
    );
}

fn relation_oid(engine: &Engine, name: &str) -> i64 {
    match engine
        .sql(&format!("SELECT '{name}'::regclass::oid::bigint"), &[])
        .unwrap()
        .value_at(0, 0)
    {
        Some(Value::Int(oid)) => *oid,
        other => panic!("{name}: {other:?}"),
    }
}

/// A NOT NULL constraint named like a CHECK constraint of its table violates `pg_constraint_conrelid_contypid_conname_index`, whose DETAIL carries the OID `heap_create_with_catalog` allocated to the relation the statement was creating: the OID after the previous table's three, which the next table's OID follows by the relation's three, the CHECK's one and the one `CreateConstraintEntry` drew before the insert failed.
#[test]
fn held_name_detail_carries_the_allocated_relation_oid() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE nn_before (a int)", &[]).unwrap();
    let before = relation_oid(&engine, "nn_before");
    let error = engine
        .sql(
            "CREATE TABLE nn_held (a int CONSTRAINT nn_x NOT NULL, CONSTRAINT nn_x CHECK (true))",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"), "{error}");
    let detail = error.detail().unwrap_or_default();
    assert_eq!(
        detail,
        format!(
            "Key (conrelid, contypid, conname)=({}, 0, nn_x) already exists.",
            before + 3
        ),
        "{error}"
    );
    engine.sql("CREATE TABLE nn_after (a int)", &[]).unwrap();
    assert_eq!(relation_oid(&engine, "nn_after"), before + 8);
}
