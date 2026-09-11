//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::view::{StoredView, StoredViewKind, ViewRegistryRead, ViewRegistryWrite};
use std::{cell::RefCell, collections::BTreeMap};
use uqa_sql::{ast::RelationPersistence, plan::UnifiedPlan};
use uqa_storage::{StorageBackendError, StorageBackendResult};

#[derive(Clone, Copy)]
enum Persistence {
    Memory,
    Success,
    Missing,
    Failure,
}
struct Fixture {
    views: RefCell<BTreeMap<RelationIdentity, StoredView>>,
    events: RefCell<Vec<&'static str>>,
    persistence: Persistence,
}
impl Fixture {
    fn new(persistence: Persistence, temporary: bool) -> Self {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS value").unwrap().remove(0))
        else {
            panic!("query fixture");
        };
        Self {
            views: RefCell::new(BTreeMap::from([(
                RelationIdentity::new("public", "v"),
                StoredView {
                    object_id: [7; 16],
                    role_owner: "owner".into(),
                    acl: None,
                    column_acls: BTreeMap::new(),
                    query: *query,
                    output_columns: Some(vec!["value".into()]),
                    persistence: if temporary {
                        RelationPersistence::Temporary
                    } else {
                        RelationPersistence::Permanent
                    },
                    options: Vec::new(),
                    kind: StoredViewKind::View,
                    materialized_rows: Vec::new(),
                    materialized_column_types: Vec::new(),
                    populated: true,
                },
            )])),
            events: RefCell::new(Vec::new()),
            persistence,
        }
    }
    fn remove(&self) -> Result<(), SQLError> {
        drop_view_state_inner(self, self, self, self, "public.v")
    }
}
impl ViewRegistryState for Fixture {
    fn views_read(&self) -> ViewRegistryRead<'_> {
        Box::new(self.views.borrow())
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        self.events.borrow_mut().push("registry write");
        Box::new(self.views.borrow_mut())
    }
}
impl ViewRemovalEvents for Fixture {
    fn rules_depending_on_relations(
        &self,
        _: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        panic!("dependency preflight precedes publication");
    }
    fn drop_rules_depending_on_relations_inner(&self, _: &[String]) -> StorageBackendResult<()> {
        panic!("dependency preflight precedes publication");
    }
    fn drop_relation_events_inner(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        assert_eq!(relation, &RelationIdentity::new("public", "v"));
        assert!(self.views.try_borrow_mut().is_ok());
        self.events.borrow_mut().push("relation events");
        Ok(())
    }
}
impl ViewRemovalPublication for Fixture {
    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<Option<bool>> {
        assert_eq!(relation, &RelationIdentity::new("public", "v"));
        assert!(
            self.views.try_borrow().is_err(),
            "persistence retains the registry write guard"
        );
        self.events.borrow_mut().push("storage delete");
        match self.persistence {
            Persistence::Memory => Ok(None),
            Persistence::Success => Ok(Some(true)),
            Persistence::Missing => Ok(Some(false)),
            Persistence::Failure => Err(StorageBackendError::Other(
                "injected deletion failure".into(),
            )),
        }
    }
}
impl CatalogPublicationChanges for Fixture {
    fn table_catalog_changed(&self) {
        panic!("view deletion publishes the registry generation only");
    }
    fn catalog_registry_changed(&self) {
        assert!(
            self.views.try_borrow_mut().is_ok(),
            "generation changes after releasing the registry write guard"
        );
        assert!(self.views.borrow().is_empty());
        self.events.borrow_mut().push("generation");
    }
}

#[test]
fn durable_view_deletion_persists_under_the_registry_guard_before_generation_publication() {
    let fixture = Fixture::new(Persistence::Success, false);
    fixture.remove().unwrap();
    assert!(fixture.views.borrow().is_empty());
    assert_eq!(
        *fixture.events.borrow(),
        [
            "relation events",
            "registry write",
            "storage delete",
            "generation"
        ]
    );
}

#[test]
fn failed_or_missing_durable_deletion_preserves_the_registry_and_generation() {
    for (persistence, message) in [
        (Persistence::Failure, "injected deletion failure"),
        (
            Persistence::Missing,
            "disappeared after dependency preflight",
        ),
    ] {
        let fixture = Fixture::new(persistence, false);
        let before =
            serde_json::to_value(fixture.views.borrow().iter().collect::<Vec<_>>()).unwrap();
        let error = fixture.remove().unwrap_err();
        assert!(error.to_string().contains(message));
        assert_eq!(
            serde_json::to_value(fixture.views.borrow().iter().collect::<Vec<_>>()).unwrap(),
            before
        );
        assert_eq!(
            *fixture.events.borrow(),
            ["relation events", "registry write", "storage delete"]
        );
        assert!(fixture.views.try_borrow_mut().is_ok());
    }
}

#[test]
fn temporary_view_deletion_skips_storage_and_memory_deletion_uses_the_loaded_registry() {
    let temporary = Fixture::new(Persistence::Failure, true);
    temporary.remove().unwrap();
    assert_eq!(
        *temporary.events.borrow(),
        ["relation events", "registry write", "generation"]
    );
    let memory = Fixture::new(Persistence::Memory, false);
    memory.remove().unwrap();
    assert!(memory.views.borrow().is_empty());
    assert_eq!(
        *memory.events.borrow(),
        [
            "relation events",
            "registry write",
            "storage delete",
            "generation"
        ]
    );
}
