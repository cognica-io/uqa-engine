//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `pg_get_ruledef` and `pg_get_triggerdef`: rules as `make_ruledef` prints them and trigger conditions as `get_rule_expr` prints them, in both forms, with constants spelled through the catalog after `ALTER TYPE ... RENAME VALUE` and `ALTER TABLE ... RENAME COLUMN`, and the rules and triggers still firing.

use uqa_engine::Engine;

fn verify_event_definitions(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/event_definition_oracle.expected.json"),
    );
}

#[test]
fn event_definitions_match_postgresql_memory() {
    verify_event_definitions(&Engine::new());
}

#[test]
fn event_definitions_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_event_definitions(
        &Engine::open(&directory.path().join("event_definitions.db")).unwrap(),
    );
}
