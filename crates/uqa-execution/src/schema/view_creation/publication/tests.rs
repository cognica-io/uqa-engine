//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::view::{StoredViewKind, ViewRegistryWrite};
use std::{
    cell::{Cell, RefCell, RefMut},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
use uqa_sql::plan::UnifiedPlan;
use uqa_storage::{StorageBackendError, StorageBackendResult, ViewRow};

struct Publication {
    views: RefCell<BTreeMap<RelationIdentity, StoredView>>,
    held: Cell<bool>,
    events: RefCell<Vec<&'static str>>,
    durable: bool,
    fail: bool,
    saved: RefCell<Vec<ViewRow>>,
}
impl Publication {
    fn new(durable: bool, fail: bool) -> Self {
        Self {
            views: RefCell::new(BTreeMap::new()),
            held: Cell::new(false),
            events: RefCell::new(Vec::new()),
            durable,
            fail,
            saved: RefCell::new(Vec::new()),
        }
    }
}
struct Guard<'a> {
    publication: &'a Publication,
    views: RefMut<'a, BTreeMap<RelationIdentity, StoredView>>,
}
impl Deref for Guard<'_> {
    type Target = BTreeMap<RelationIdentity, StoredView>;
    fn deref(&self) -> &Self::Target {
        &self.views
    }
}
impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.views
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.publication.held.set(false);
        self.publication.events.borrow_mut().push("unlock");
    }
}
impl ViewPublication for Publication {
    fn has_catalog(&self) -> bool {
        self.events.borrow_mut().push("catalog");
        self.durable
    }
    fn save_view(&self, row: &ViewRow) -> StorageBackendResult<()> {
        self.events.borrow_mut().push(if self.held.get() {
            "save_locked"
        } else {
            "save_unlocked"
        });
        if self.fail {
            return Err(StorageBackendError::Other("write failure".into()));
        }
        self.saved.borrow_mut().push(row.clone());
        Ok(())
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        assert!(!self.held.replace(true));
        self.events.borrow_mut().push("lock");
        Box::new(Guard {
            publication: self,
            views: self.views.borrow_mut(),
        })
    }
}
impl CatalogPublicationChanges for Publication {
    fn table_catalog_changed(&self) {
        panic!("view publication must not change the table epoch");
    }
    fn catalog_registry_changed(&self) {
        assert!(!self.held.get());
        assert!(!self.views.borrow().is_empty());
        self.events.borrow_mut().push("publish");
    }
}
fn definition(kind: StoredViewKind, persistence: RelationPersistence) -> StoredView {
    let statement = uqa_sql::compile("SELECT 1 AS value").unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query fixture");
    };
    StoredView {
        object_id: [7; 16],
        role_owner: "view_owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["value".into()]),
        persistence,
        options: Vec::new(),
        kind,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}

#[test]
fn view_publication_preserves_distinct_regular_and_materialized_guard_boundaries() {
    let relation = RelationIdentity::new("public", "v");
    let regular = Publication::new(true, false);
    publish_regular_view(
        &regular,
        &regular,
        relation.clone(),
        definition(StoredViewKind::View, RelationPersistence::Permanent),
        "public.v",
    )
    .unwrap();
    assert_eq!(
        *regular.events.borrow(),
        ["lock", "catalog", "save_locked", "unlock", "publish"]
    );
    assert_eq!(regular.views.borrow()[&relation].object_id, [7; 16]);
    assert_eq!(regular.saved.borrow()[0].role_owner, "view_owner");
    let materialized = Publication::new(true, false);
    publish_materialized_view(
        &materialized,
        &materialized,
        relation,
        definition(StoredViewKind::Materialized, RelationPersistence::Permanent),
        "public.v",
    )
    .unwrap();
    assert_eq!(
        *materialized.events.borrow(),
        ["catalog", "save_unlocked", "lock", "unlock", "publish"]
    );
}

#[test]
fn persistence_failure_keeps_the_previous_view_and_suppresses_publication() {
    for kind in [StoredViewKind::View, StoredViewKind::Materialized] {
        let publication = Publication::new(true, true);
        let relation = RelationIdentity::new("public", "v");
        let mut old = definition(kind, RelationPersistence::Permanent);
        old.object_id = [1; 16];
        publication.views.borrow_mut().insert(relation.clone(), old);
        let operation = match kind {
            StoredViewKind::View => publish_regular_view,
            StoredViewKind::Materialized => publish_materialized_view,
        };
        let error = operation(
            &publication,
            &publication,
            relation.clone(),
            definition(kind, RelationPersistence::Permanent),
            "public.v",
        )
        .unwrap_err();
        assert!(error.to_string().contains("write failure"));
        assert_eq!(publication.views.borrow()[&relation].object_id, [1; 16]);
        assert!(!publication.held.get());
        assert!(!publication.events.borrow().contains(&"publish"));
        if kind == StoredViewKind::Materialized {
            assert!(!publication.events.borrow().contains(&"lock"));
        }
    }
}

#[test]
fn temporary_and_memory_views_publish_without_durable_writes() {
    for (durable, persistence, expected) in [
        (
            true,
            RelationPersistence::Temporary,
            vec!["lock", "unlock", "publish"],
        ),
        (
            false,
            RelationPersistence::Permanent,
            vec!["lock", "catalog", "unlock", "publish"],
        ),
    ] {
        let publication = Publication::new(durable, true);
        publish_regular_view(
            &publication,
            &publication,
            RelationIdentity::new("public", "v"),
            definition(StoredViewKind::View, persistence),
            "public.v",
        )
        .unwrap();
        assert_eq!(*publication.events.borrow(), expected);
        assert!(publication.saved.borrow().is_empty());
    }
}
