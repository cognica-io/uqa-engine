//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared plan selection, actual statistics, and session overrides against `PostgreSQL`.

use uqa_engine::Engine;

const SELECTION: &str =
    include_str!("../../../../tests/parity/pg18/prepared_plan_selection_oracle.expected.json");
const COST: &str =
    include_str!("../../../../tests/parity/pg18/prepared_plan_cost_oracle.expected.json");
const SETTINGS: &str =
    include_str!("../../../../tests/parity/pg18/prepared_plan_settings_oracle.expected.json");

#[test]
fn prepared_plan_selection_matches_postgresql_memory() {
    super::parameters::verify_parameters(&Engine::new(), SELECTION);
}

#[test]
fn prepared_plan_selection_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("selection.db")).unwrap();
    super::parameters::verify_parameters(&engine, SELECTION);
}

#[test]
fn prepared_plan_cost_matches_postgresql_memory() {
    super::parameters::verify_parameters(&Engine::new(), COST);
}

#[test]
fn prepared_plan_cost_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("cost.db")).unwrap();
    super::parameters::verify_parameters(&engine, COST);
}

#[test]
fn prepared_plan_settings_match_postgresql() {
    super::parameters::verify_parameters(&Engine::new(), SETTINGS);
}

#[test]
fn prepared_plan_types_match_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!("../../../../tests/parity/pg18/prepared_plan_types_oracle.expected.json"),
    );
}
