//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    binding::stored_relations::StoredQuerySequences, catalog::resolution::RelationLookupMode,
};
use std::{collections::BTreeSet, sync::Mutex};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn stored_sequence_candidates_preserve_order_quoted_names_and_controlled_failures() {
    let candidates = [
        RelationIdentity::new("absent", "counter"),
        RelationIdentity::new("a.b", "c\"d"),
        RelationIdentity::new("later", "counter"),
    ];
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut visits = Vec::new();
    let name = resolve_loaded_query_sequence_with_control(
        "counter",
        &candidates,
        |candidate| {
            visits.push(candidate.schema.clone());
            candidate.schema != "absent"
        },
        &control,
    )
    .unwrap();
    assert_eq!(name.as_str(), "\"a.b\".\"c\"\"d\"");
    assert_eq!(visits, ["absent", "a.b"]);
    assert_eq!(
        super::super::super::resolve_loaded_query_sequence(
            "counter",
            candidates.to_vec(),
            |candidate| candidate.schema != "absent"
        )
        .unwrap(),
        *name
    );
    drop(name);
    assert_eq!(memory.used(), 0);
    let blocker = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        select_query_sequence_with_control(&candidates, |_| true, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(blocker);
    let mut calls = 0;
    assert_eq!(
        select_query_sequence_with_control(
            &candidates,
            |_| {
                calls += 1;
                token.cancel();
                false
            },
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(calls, 1);
    assert_eq!(memory.used(), 0);
}

struct External(Mutex<Option<String>>);
impl StoredQuerySequences for External {
    fn query_sequence(&self, _: &str) -> Result<String, String> {
        Ok(self.0.lock().unwrap().take().unwrap())
    }
    fn loaded_query_sequence(&self, _: &str) -> Result<String, String> {
        Err("not loaded".into())
    }
}

#[test]
fn stored_sequence_external_names_transfer_capacity_and_keep_legacy_diagnostics() {
    let mut name = String::with_capacity(4096);
    name.push_str("public.counter");
    let pointer = name.as_ptr();
    let capacity = name.capacity();
    let sequences = External(Mutex::new(Some(name)));
    let transitions = BTreeSet::new();
    let context = StoredQueryBindingContext {
        relations: &(),
        lookup_mode: RelationLookupMode::Dynamic,
        sequences: &sequences,
        temporary_schema: "pg_temp_1",
        transition_relations: &transitions,
    };
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sibling = memory.reserve(37).unwrap();
    let name = context
        .canonical_sequence_with_control("counter", "SQL routine body", false, &control)
        .unwrap();
    assert_eq!(name.as_ptr(), pointer);
    assert_eq!(memory.used(), capacity + 37);
    drop(name);
    assert_eq!(memory.used(), 37);
    for control in [control, ProductionControl::uncontrolled()] {
        let error = context
            .canonical_sequence_with_control("counter", "SQL routine body", true, &control)
            .unwrap_err();
        assert!(
            matches!(error, SQLError::Unsupported(message) if message == "SQL routine body sequence reference `counter`: not loaded")
        );
    }
    drop(sibling);
    assert_eq!(memory.used(), 0);
}
