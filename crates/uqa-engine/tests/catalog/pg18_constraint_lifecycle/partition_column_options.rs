//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the column options of `CREATE TABLE ... PARTITION OF`: options on columns the parent lacks and repeated options, defaults and generation expressions that replace the parent's for the partition and its partitions, `NOT NULL`, `CHECK`, `PRIMARY KEY`, `UNIQUE` and `REFERENCES` options, generated parent columns kept, overridden or conflicting, identity on a partition, and the defaults and bound checked after the merge.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_partition_column_options(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!(
            "../../../../../tests/parity/pg18/partition_column_options_oracle.expected.json"
        ),
    );
}

#[test]
fn partition_column_options_match_postgresql_memory() {
    verify_partition_column_options(&Engine::new());
}

#[test]
fn partition_column_options_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_partition_column_options(
        &Engine::open(&directory.path().join("partition-column-options.db")).unwrap(),
    );
}
