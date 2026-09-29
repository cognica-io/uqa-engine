//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for partition bounds: values coerced to their key types and evaluated once, bound expression restrictions, `pg_get_expr` rendering by key type, strategy and width diagnostics, overlap and default-partition conflicts, the hash modulus chain, routing failures with their partition key, and partition key expression checks.

use uqa_engine::Engine;

fn verify_partition_bounds(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/partition_bounds_oracle.expected.json"),
    );
}

#[test]
fn partition_bounds_match_postgresql_memory() {
    verify_partition_bounds(&Engine::new());
}

#[test]
fn partition_bounds_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_partition_bounds(&Engine::open(&directory.path().join("partitions.db")).unwrap());
}
