//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;
use uqa_storage::{SequenceOwner, SequenceOwnerDependency};

fn owner(engine: &Engine) -> SequenceOwner {
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let column_object_id = table.columns.read()[0].object_id.unwrap();
    SequenceOwner {
        table_object_id: table.object_id(),
        column_object_id,
        dependency: SequenceOwnerDependency::Automatic,
    }
}
#[test]
fn unchanged_and_conflicting_implicit_owners_leave_the_actual_allocation_state_untouched() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE items(id serial)", &[]).unwrap();
    let before = engine.sequence_state("items_id_seq").unwrap().unwrap().1;
    let original = before.owner.unwrap();
    let context = engine.sequence_owner_publication_context();
    context
        .attach_sequence_owner_identity("items_id_seq", original)
        .unwrap();
    let other = SequenceOwner {
        column_object_id: [9; 16],
        ..original
    };
    let error = context
        .attach_sequence_owner_identity("items_id_seq", other)
        .unwrap_err();
    assert!(
        matches!(error,SQLError::Internal(message) if message=="implicit sequence `public.items_id_seq` already has another owner")
    );
    assert_eq!(
        engine.sequence_state("items_id_seq").unwrap().unwrap().1,
        before
    );
}

#[test]
fn owner_attachment_rollback_keeps_value_progress_in_the_original_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("allocation.db");
    let engine = Engine::open(&path).unwrap();
    engine
        .sql(
            "CREATE TABLE items(id integer); CREATE SEQUENCE ids START WITH 17 CACHE 3",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("ids").unwrap(), 17);
    let before = engine.sequence_state("ids").unwrap().unwrap().1;
    let owner = owner(&engine);
    engine.begin().unwrap();
    engine
        .with_implicit_transaction(|engine| {
            engine
                .sequence_owner_publication_context()
                .attach_sequence_owner_identity("ids", owner)
        })
        .unwrap();
    assert_eq!(engine.nextval("ids").unwrap(), 20);
    engine.rollback().unwrap();
    assert_eq!(engine.nextval("ids").unwrap(), 21);
    let after = engine.sequence_state("ids").unwrap().unwrap().1;
    assert_eq!(after.definition_generation, before.definition_generation);
    assert_eq!(after.current, 22);
    assert!(after.owner.is_none());
    assert!(engine
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_sequence_rows()
        .unwrap()[0]
        .owner
        .is_none());
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(reopened.sequence_state("ids").unwrap().unwrap().1, after);
    assert_eq!(reopened.nextval("ids").unwrap(), 23);
}

#[test]
fn owner_attachment_reopens_with_the_same_sequence_identity_and_allocation_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("attachment.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE items(id integer); CREATE SEQUENCE ids START WITH 17 INCREMENT BY 3 CACHE 4",&[]).unwrap();
    let before = engine.sequence_state("ids").unwrap().unwrap().1;
    let relation = RelationIdentity::new("public", "ids");
    let object_id = engine.durable.sequence_object_ids.read()[&relation];
    let owner = owner(&engine);
    engine
        .with_implicit_transaction(|engine| {
            engine
                .sequence_owner_publication_context()
                .attach_sequence_owner_identity("ids", owner)
        })
        .unwrap();
    let after = engine.sequence_state("ids").unwrap().unwrap().1;
    assert_eq!(after.definition_generation, before.definition_generation);
    assert_ne!(after.definition_generation, [0; 16]);
    let expected = crate::SequenceState {
        owner: Some(owner),
        ..before
    };
    assert_eq!(after, expected);
    assert_eq!(
        engine.durable.sequence_object_ids.read()[&relation],
        object_id
    );
    assert_eq!(
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_sequence_rows()
            .unwrap()[0]
            .owner,
        Some(owner)
    );
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(reopened.sequence_state("ids").unwrap().unwrap().1, after);
    assert_eq!(
        reopened.durable.sequence_object_ids.read()[&relation],
        object_id
    );
    assert_eq!(reopened.nextval("ids").unwrap(), 17);
}

#[test]
fn loaded_owner_validation_rejects_stale_metadata_without_repairing_the_live_registry() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE items(id serial)", &[]).unwrap();
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let columns = table.columns.read().clone();
    let relation = RelationIdentity::new("public", "items_id_seq");
    let before = engine.durable.sequences.read()[&relation];
    engine
        .durable
        .sequences
        .write()
        .get_mut(&relation)
        .unwrap()
        .owner = None;
    let error = engine
        .sequence_owner_publication_context()
        .validate_implicit_sequence_owners_for_columns("public.items", table.object_id(), &columns)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("has stale owner metadata that requires an initial-open migration"));
    assert!(engine.durable.sequences.read()[&relation].owner.is_none());
    engine.durable.sequences.write().insert(relation, before);
    engine
        .sequence_owner_publication_context()
        .validate_implicit_sequence_owners_for_columns("public.items", table.object_id(), &columns)
        .unwrap();
}
