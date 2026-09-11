//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

struct UnreachablePublication;
impl SequenceRemovalPublication for UnreachablePublication {
    fn remove_state(&self, _: &str) -> Result<bool, String> {
        panic!("all identity-owner checks must finish before any sequence removal")
    }
}
#[test]
fn multi_sequence_identity_preflight_prevents_removing_an_earlier_unowned_sequence() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE plain; CREATE TABLE items(id bigint GENERATED ALWAYS AS IDENTITY)",
            &[],
        )
        .unwrap();
    let plain = engine.sequence_state("plain").unwrap().unwrap().1;
    let identity = engine.sequence_state("items_id_seq").unwrap().unwrap().1;
    let error = engine
        .with_implicit_transaction(|engine| {
            let mut context = engine.sequence_removal_context();
            context.publication = &UnreachablePublication;
            context.drop_sequences(&["public.plain".into(), "public.items_id_seq".into()], true)
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"));
    assert!(error
        .to_string()
        .contains("column id of table public.items requires it"));
    assert_eq!(engine.sequence_state("plain").unwrap().unwrap().1, plain);
    assert_eq!(
        engine.sequence_state("items_id_seq").unwrap().unwrap().1,
        identity
    );
    assert!(engine.take_sql_notices().is_empty());
}
struct FailedPublication<'a> {
    engine: &'a Engine,
    reached: Cell<bool>,
}
impl SequenceRemovalPublication for FailedPublication<'_> {
    fn remove_state(&self, name: &str) -> Result<bool, String> {
        assert_eq!(name, "public.ids");
        let table =
            self.engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
        assert!(
            table.columns.read()[0].default.is_none(),
            "dependent default is published before removing the sequence"
        );
        assert!(
            !self
                .engine
                .durable
                .views
                .read()
                .contains_key(&RelationIdentity::new("public", "dependent")),
            "dependent views are removed before sequence state"
        );
        assert!(self
            .engine
            .durable
            .sequences
            .read()
            .contains_key(&RelationIdentity::new("public", "ids")));
        assert_eq!(
            self.engine
                .storage
                .catalog
                .as_ref()
                .unwrap()
                .load_sequence_rows()
                .unwrap()
                .len(),
            1
        );
        assert!(
            self.engine.runtime.notices.lock().is_empty(),
            "final cascade notices must follow successful sequence publication"
        );
        self.reached.set(true);
        Err("injected sequence removal failure".into())
    }
}
fn assert_restored(engine: &Engine) {
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    assert!(table.columns.read()[0].default.is_some());
    assert!(engine
        .durable
        .views
        .read()
        .contains_key(&RelationIdentity::new("public", "dependent")));
    assert!(engine.sequence_state("ids").unwrap().is_some());
}
#[test]
fn failed_sequence_publication_rolls_back_prior_native_default_and_view_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("removal.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE SEQUENCE ids; CREATE TABLE items(id bigint DEFAULT nextval('ids')); CREATE VIEW dependent AS SELECT nextval('ids') AS id",&[]).unwrap();
    {
        let publication = FailedPublication {
            engine: &engine,
            reached: Cell::new(false),
        };
        let error = engine
            .with_implicit_transaction(|engine| {
                let mut context = engine.sequence_removal_context();
                context.publication = &publication;
                context.drop_sequences(&["public.ids".into()], true)
            })
            .unwrap_err();
        assert!(
            matches!(error,SQLError::Internal(message) if message=="injected sequence removal failure")
        );
        assert!(publication.reached.get());
        assert_restored(&engine);
        assert!(engine.take_sql_notices().is_empty());
    }
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_restored(&reopened);
}
#[test]
fn native_sequence_removal_clears_only_matching_registry_and_session_cache_identities() {
    for removed_is_last in [false, true] {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE SEQUENCE ids CACHE 5; CREATE SEQUENCE other CACHE 5",
                &[],
            )
            .unwrap();
        assert_eq!(engine.nextval("ids").unwrap(), 1);
        assert_eq!(engine.nextval("other").unwrap(), 1);
        if removed_is_last {
            assert_eq!(engine.nextval("ids").unwrap(), 2);
        }
        let relation = RelationIdentity::new("public", "ids");
        let object_id = engine.durable.sequence_object_ids.read()[&relation];
        let mut expected_caches = engine.session.sequence_caches.lock().clone();
        expected_caches.retain(|_, cache| cache.object_id != object_id);
        assert!(!expected_caches.is_empty());
        let mut expected_currvals = engine.session.state.read().sequence_currvals.clone();
        expected_currvals.retain(|_, value| value.object_id != object_id);
        let expected_last = if removed_is_last {
            None
        } else {
            engine.session.state.read().last_sequence.clone()
        };
        assert!(engine.drop_sequence("ids").unwrap());
        assert!(!engine.durable.sequences.read().contains_key(&relation));
        assert!(!engine
            .durable
            .sequence_object_ids
            .read()
            .contains_key(&relation));
        assert!(!engine
            .durable
            .sequence_persistence
            .read()
            .contains_key(&relation));
        assert!(!engine
            .durable
            .sequence_security
            .read()
            .contains_key(&relation));
        assert!(*engine.session.sequence_caches.lock() == expected_caches);
        assert!(engine.session.state.read().sequence_currvals == expected_currvals);
        assert!(engine.session.state.read().last_sequence == expected_last);
        assert!(!engine.drop_sequence("ids").unwrap());
        assert_eq!(engine.currval("other").unwrap(), 1);
        if removed_is_last {
            assert_eq!(
                engine.lastval().unwrap_err(),
                "lastval is not yet defined in this session"
            );
        } else {
            assert_eq!(engine.lastval().unwrap(), 1);
        }
        assert_eq!(engine.nextval("other").unwrap(), 2);
    }
}
