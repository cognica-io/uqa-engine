//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use serde_json::Value as Json;

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn empty_prepared_messages_match_postgresql(#[case] persistent: bool) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_empty_oracle.expected.json"
    ))
    .unwrap();
    assert!(reference["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("empty.db")).unwrap()
    } else {
        Engine::new()
    };
    super::reference::verify(&engine, &reference);
}
