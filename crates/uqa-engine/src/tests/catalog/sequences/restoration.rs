//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::{cell::Cell, collections::BTreeMap};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::sequence::restoration::{
    RestoredSequenceRegistry, SequencePersistenceRead, SequenceRestoreRegistry,
};
use uqa_execution::catalog::sequence::{restoration::restore_sequence_rows, SequenceState};
use uqa_sql::{ast::RelationPersistence, catalog::security::SequenceSecurity};
use uqa_storage::SequenceRow;

#[derive(Debug, PartialEq)]
struct Snapshot {
    sequences: BTreeMap<RelationIdentity, SequenceState>,
    object_ids: BTreeMap<RelationIdentity, [u8; 16]>,
    persistence: BTreeMap<RelationIdentity, RelationPersistence>,
    security: BTreeMap<RelationIdentity, SequenceSecurity>,
}
fn snapshot(engine: &Engine) -> Snapshot {
    Snapshot {
        sequences: engine.durable.sequences.read().clone(),
        object_ids: engine.durable.sequence_object_ids.read().clone(),
        persistence: engine.durable.sequence_persistence.read().clone(),
        security: engine.durable.sequence_security.read().clone(),
    }
}
fn setup() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE stale_ids CACHE 7; CREATE TEMP SEQUENCE session_ids CACHE 3",
            &[],
        )
        .unwrap();
    engine
}
fn incoming(engine: &Engine, name: &str, id: [u8; 16]) -> SequenceRow {
    let state = engine.sequence_state("stale_ids").unwrap().unwrap().1;
    Engine::sequence_row(
        name,
        id,
        state,
        RelationPersistence::Permanent,
        &SequenceSecurity {
            role_owner: "uqa".into(),
            acl: None,
        },
    )
    .unwrap()
}
#[test]
fn corrupt_later_sequence_rows_leave_all_actual_registries_and_temporary_state_unchanged() {
    let engine = setup();
    let before = snapshot(&engine);
    let valid = incoming(&engine, "public.valid", [8; 16]);
    for kind in [
        "owner",
        "zero identity",
        "duplicate identity",
        "persistence",
        "generation",
    ] {
        let mut bad = incoming(&engine, "public.broken", [9; 16]);
        match kind {
            "owner" => bad.role_owner.clear(),
            "zero identity" => bad.object_id = [0; 16],
            "duplicate identity" => bad.object_id = valid.object_id,
            "persistence" => bad.persistence = "t".into(),
            "generation" => bad.definition_generation = [0; 16],
            _ => unreachable!(),
        }
        let error =
            restore_sequence_rows(&engine.sequence_restore_context(), vec![valid.clone(), bad])
                .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("corrupt sequence `public.broken`"),
            "{kind}: {error}"
        );
        assert_eq!(snapshot(&engine), before, "{kind}");
    }
}
struct PublicationObserver<'a> {
    engine: &'a Engine,
    calls: Cell<usize>,
}
impl SequenceRestoreRegistry for PublicationObserver<'_> {
    fn persistence(&self) -> SequencePersistenceRead<'_> {
        SequenceRestoreRegistry::persistence(self.engine)
    }
    fn install(&self, registry: RestoredSequenceRegistry) {
        assert!(!self.engine.durable.sequences.is_locked());
        assert!(!self.engine.durable.sequence_object_ids.is_locked());
        assert!(!self.engine.durable.sequence_persistence.is_locked());
        assert!(!self.engine.durable.sequence_security.is_locked());
        self.calls.set(self.calls.get() + 1);
        SequenceRestoreRegistry::install(self.engine, registry);
    }
}
#[test]
fn successful_restore_releases_metadata_guards_and_preserves_only_the_live_temporary_entries() {
    let engine = setup();
    let before = snapshot(&engine);
    let temporary = before
        .persistence
        .iter()
        .find(|(_, p)| **p == RelationPersistence::Temporary)
        .unwrap()
        .0
        .clone();
    let row = incoming(&engine, "public.restored", [8; 16]);
    let relation = row.relation.clone();
    let object_id = row.object_id;
    let publication = PublicationObserver {
        engine: &engine,
        calls: Cell::new(0),
    };
    let mut context = engine.sequence_restore_context();
    context.registry = &publication;
    restore_sequence_rows(&context, vec![row]).unwrap();
    assert_eq!(publication.calls.get(), 1);
    let after = snapshot(&engine);
    assert_eq!(after.sequences.len(), 2);
    assert_eq!(after.sequences[&temporary], before.sequences[&temporary]);
    assert_eq!(after.object_ids[&temporary], before.object_ids[&temporary]);
    assert_eq!(after.security[&temporary], before.security[&temporary]);
    assert_eq!(
        after.persistence[&temporary],
        RelationPersistence::Temporary
    );
    assert_eq!(after.object_ids[&relation], object_id);
    assert_eq!(after.persistence[&relation], RelationPersistence::Permanent);
    assert!(after.sequences.contains_key(&relation));
    assert!(!after
        .sequences
        .contains_key(&RelationIdentity::new("public", "stale_ids")));
    assert_eq!(after.object_ids.len(), 2);
    assert_eq!(after.security.len(), 2);
    assert_eq!(after.persistence.len(), 2);
}
