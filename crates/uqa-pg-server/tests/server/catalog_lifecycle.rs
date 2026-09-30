//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The type lifecycle, the catalog dependencies, dependency-aware drops and `ALTER COLUMN` checks over the `PostgreSQL` wire protocol: command tags, result metadata, errors with their DETAIL and HINT, and notices.

use super::client::compare_oracle;

#[test]
fn type_lifecycle_oracle_matches_postgresql_over_tcp() {
    compare_oracle(include_str!(
        "../../../../tests/parity/pg18/type_lifecycle_oracle.expected.json"
    ));
}

#[test]
fn catalog_dependency_oracle_matches_postgresql_over_tcp() {
    compare_oracle(include_str!(
        "../../../../tests/parity/pg18/catalog_dependencies_oracle.expected.json"
    ));
}

#[test]
fn drop_dependency_oracle_matches_postgresql_over_tcp() {
    compare_oracle(include_str!(
        "../../../../tests/parity/pg18/drop_dependencies_oracle.expected.json"
    ));
}

#[test]
fn alter_column_diagnostics_oracle_matches_postgresql_over_tcp() {
    compare_oracle(include_str!(
        "../../../../tests/parity/pg18/alter_column_diagnostics_oracle.expected.json"
    ));
}
