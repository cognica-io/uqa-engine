//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn prepared_call_matches_postgresql(#[case] persistent: bool) {
    let reference = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_call_oracle.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("calls.db")).unwrap()
    } else {
        Engine::new()
    };
    super::reference::verify(&engine, &reference);
}
