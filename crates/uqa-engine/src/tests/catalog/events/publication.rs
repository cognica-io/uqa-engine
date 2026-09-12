//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use parking_lot::{MappedRwLockWriteGuard, Mutex};
use std::{
    cell::Cell,
    ops::{Deref, DerefMut},
};
use uqa_core::RelationIdentity;
use uqa_execution::schema::{
    events::{
        context::ConstraintTriggerEvents, EventCatalogGuards, EventCatalogPublication,
        RuleCatalogWrite, TriggerCatalogWrite,
    },
    publication::dependencies::CatalogPublicationChanges,
};
use uqa_sql::{
    catalog::{
        constraints::ConstraintIdentity,
        events::{RuleCatalog, TriggerCatalog},
    },
    SQLError,
};

struct Publication<'a> {
    engine: &'a Engine,
    fail_rules: bool,
    triggers_held: Cell<bool>,
    rules_held: Cell<bool>,
    events: Mutex<Vec<&'static str>>,
}
impl Publication<'_> {
    fn guards_held(&self) {
        assert!(self.triggers_held.get());
        assert!(self.rules_held.get());
    }
    fn guards_released(&self) {
        assert!(!self.triggers_held.get());
        assert!(!self.rules_held.get());
    }
}
// Track the lifetime of the real Engine write guards without changing CatalogCell's API.
struct TrackedWrite<'a, T> {
    guard: Option<MappedRwLockWriteGuard<'a, T>>,
    held: &'a Cell<bool>,
}
impl<T> Deref for TrackedWrite<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().unwrap()
    }
}
impl<T> DerefMut for TrackedWrite<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_deref_mut().unwrap()
    }
}
impl<T> Drop for TrackedWrite<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.held.set(false);
    }
}
impl EventCatalogGuards for Publication<'_> {
    fn triggers(&self) -> TriggerCatalogWrite<'_> {
        let guard = self.engine.durable.triggers.write();
        assert!(!self.triggers_held.replace(true));
        Box::new(TrackedWrite {
            guard: Some(guard),
            held: &self.triggers_held,
        })
    }
    fn rules(&self) -> RuleCatalogWrite<'_> {
        let guard = self.engine.durable.rules.write();
        assert!(!self.rules_held.replace(true));
        Box::new(TrackedWrite {
            guard: Some(guard),
            held: &self.rules_held,
        })
    }
}
impl EventCatalogPublication for Publication<'_> {
    fn persist_triggers(&self, _: &TriggerCatalog) -> Result<(), SQLError> {
        self.guards_held();
        self.events.lock().push("persist-triggers");
        Ok(())
    }
    fn persist_rules(&self, _: &RuleCatalog) -> Result<(), SQLError> {
        self.guards_held();
        self.events.lock().push("persist-rules");
        if self.fail_rules {
            Err(SQLError::Internal(
                "injected event persistence failure".into(),
            ))
        } else {
            Ok(())
        }
    }
}
impl ConstraintTriggerEvents for Publication<'_> {
    fn forget(&self, identity: &ConstraintIdentity) {
        self.guards_released();
        assert!(!self
            .engine
            .durable
            .triggers
            .read()
            .contains_key(&identity.relation));
        assert!(!self
            .engine
            .durable
            .rules
            .read()
            .contains_key(&identity.relation));
        self.events.lock().push("forget-pending");
        self.engine.forget_constraint_trigger_events(identity);
    }
    fn rename_trigger(&self, _: &ConstraintIdentity, _: &str) {
        panic!("relation drop must not rename triggers")
    }
    fn rename_constraint(&self, _: &ConstraintIdentity, _: &str) {
        panic!("relation drop must not rename constraints")
    }
}
impl CatalogPublicationChanges for Publication<'_> {
    fn table_catalog_changed(&self) {
        panic!("event publication changes only the registry epoch")
    }
    fn catalog_registry_changed(&self) {
        self.guards_released();
        self.events.lock().push("registry-epoch");
        self.engine.note_catalog_registry_changed();
    }
}
fn engine_with_deferred_event() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE TABLE dependency_items(id integer); CREATE TABLE dependency_audit(id integer); CREATE FUNCTION dependency_handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO dependency_audit VALUES (NEW.id); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER dependency_check AFTER INSERT ON dependency_items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION dependency_handler(); CREATE RULE dependency_rule AS ON DELETE TO dependency_items DO NOTHING; BEGIN; INSERT INTO dependency_items VALUES (1)",&[]).unwrap();
    engine
}

#[test]
fn failed_relation_event_persistence_preserves_both_registries_and_pending_trigger() {
    let engine = engine_with_deferred_event();
    let observer = Publication {
        engine: &engine,
        fail_rules: true,
        triggers_held: Cell::new(false),
        rules_held: Cell::new(false),
        events: Mutex::new(Vec::new()),
    };
    let relation = RelationIdentity::new("public", "dependency_items");
    let mut context = engine.event_lifecycle_context();
    context.catalog.registry = &observer;
    context.catalog.publication = &observer;
    context.catalog.changes = &observer;
    context.pending = &observer;
    let error = context.drop_relation_events_inner(&relation).unwrap_err();
    assert!(error
        .to_string()
        .contains("injected event persistence failure"));
    assert_eq!(
        *observer.events.lock(),
        ["persist-triggers", "persist-rules"]
    );
    observer.guards_released();
    assert!(engine.durable.triggers.read().contains_key(&relation));
    assert!(engine.durable.rules.read().contains_key(&relation));
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM dependency_audit", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn relation_event_publication_releases_guards_before_forgetting_pending_events_and_epoch() {
    let engine = engine_with_deferred_event();
    let observer = Publication {
        engine: &engine,
        fail_rules: false,
        triggers_held: Cell::new(false),
        rules_held: Cell::new(false),
        events: Mutex::new(Vec::new()),
    };
    let mut context = engine.event_lifecycle_context();
    context.catalog.registry = &observer;
    context.catalog.publication = &observer;
    context.catalog.changes = &observer;
    context.pending = &observer;
    context
        .drop_relation_events_inner(&RelationIdentity::new("public", "dependency_items"))
        .unwrap();
    assert_eq!(
        *observer.events.lock(),
        [
            "persist-triggers",
            "persist-rules",
            "forget-pending",
            "registry-epoch"
        ]
    );
    engine.sql("COMMIT", &[]).unwrap();
    assert!(engine
        .sql("SELECT id FROM dependency_audit", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn invalid_event_column_rewrite_preserves_registry_before_any_persistence() {
    let engine = engine_with_deferred_event();
    let relation = RelationIdentity::new("public", "dependency_items");
    engine
        .durable
        .triggers
        .write()
        .get_mut(&relation)
        .unwrap()
        .get_mut("dependency_check")
        .unwrap()
        .definition
        .when = Some(uqa_sql::ast::Expr::Star);
    let before = serde_json::to_string(&engine.event_lookup_context().list_triggers()).unwrap();
    let observer = Publication {
        engine: &engine,
        fail_rules: false,
        triggers_held: Cell::new(false),
        rules_held: Cell::new(false),
        events: Mutex::new(Vec::new()),
    };
    let mut context = engine.event_lifecycle_context();
    context.catalog.registry = &observer;
    context.catalog.publication = &observer;
    context.catalog.changes = &observer;
    context.pending = &observer;
    let error = context
        .rename_event_column_inner("public.dependency_items", "id", "new_id")
        .unwrap_err();
    assert!(error.to_string().contains("schema expression contains `*`"));
    assert!(observer.events.lock().is_empty());
    assert_eq!(
        serde_json::to_string(&engine.event_lookup_context().list_triggers()).unwrap(),
        before
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}
