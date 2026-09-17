//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::view::{StoredView, ViewRegistryRead, ViewRegistryWrite},
    row_locks::{RowLockManager, ScopedRelationLock},
};
use std::{cell::RefCell, collections::BTreeMap};
use uqa_core::CancellationToken;
use uqa_sql::{ast::RelationPersistence, catalog::view::StoredViewKind, plan::UnifiedPlan};

fn view(id: u8, source: &str) -> StoredView {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT * FROM public.{source}"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("view query")
    };
    uqa_sql::binding::view_dependencies::bind_query_plan_relations(
        &mut query,
        &BTreeSet::new(),
        &mut |name| Ok::<_, String>(name.to_string()),
    )
    .unwrap();
    StoredView {
        object_id: [id; 16],
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: None,
        persistence: RelationPersistence::Permanent,
        options: Vec::new(),
        kind: StoredViewKind::View,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}

type Registry = BTreeMap<RelationIdentity, StoredView>;
type Change = Box<dyn FnOnce(&mut Registry)>;
struct Session {
    views: RefCell<Registry>,
    change: RefCell<Option<Change>>,
    locks: RowLockManager,
    cancellation: CancellationToken,
}

fn name(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}

impl Session {
    fn new(change: impl FnOnce(&mut Registry) + 'static) -> Self {
        Self {
            views: RefCell::new(BTreeMap::from([
                (name("dependent"), view(1, "root")),
                (name("outer_view"), view(2, "dependent")),
            ])),
            change: RefCell::new(Some(Box::new(change))),
            locks: RowLockManager::new(),
            cancellation: CancellationToken::new(),
        }
    }
    fn peer_acquires(&self, name: &str, mode: RelationLockMode) -> bool {
        self.locks
            .try_acquire_relation(2, self.locks.table_key(name), mode, 0, &self.cancellation)
            .unwrap()
    }
}

impl ViewRegistryState for Session {
    fn views_read(&self) -> ViewRegistryRead<'_> {
        Box::new(self.views.borrow())
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        Box::new(self.views.borrow_mut())
    }
}
impl RelationLockSession for Session {
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        _: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.locks
            .acquire_scoped_relation(
                1,
                self.locks.table_key(name),
                mode,
                (0, 1),
                &self.cancellation,
            )
            .map(Some)
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        if let Some(change) = self.change.borrow_mut().take() {
            change(&mut self.views.borrow_mut());
        }
        Ok(())
    }
}

#[test]
fn removed_dependency_releases_only_the_provisional_upgrade_and_keeps_its_descendants() {
    let session = Session::new(|views| {
        views.insert(name("dependent"), view(1, "other"));
    });
    session
        .acquire("public.dependent", RelationLockMode::AccessShare, false)
        .unwrap()
        .unwrap()
        .retain();
    assert!(
        lock_dependent_views(&session, &session, &["public.root".into()])
            .unwrap()
            .is_empty()
    );
    assert!(!session.peer_acquires("public.dependent", RelationLockMode::AccessExclusive));
    assert!(session.peer_acquires("public.dependent", RelationLockMode::AccessShare));
    assert!(session.peer_acquires("public.outer_view", RelationLockMode::AccessExclusive));
}

#[test]
fn renamed_dependents_follow_identity_and_traverse_fresh_sources_without_adopting_the_old_name() {
    let session = Session::new(|views| {
        let moved = views.remove(&name("dependent")).unwrap();
        views.insert(name("moved"), moved);
        views.insert(name("dependent"), view(3, "other"));
        views.insert(name("outer_view"), view(2, "moved"));
    });
    assert_eq!(
        lock_dependent_views(&session, &session, &["public.root".into()]).unwrap(),
        ["public.moved", "public.outer_view"]
    );
    assert!(session.peer_acquires("public.dependent", RelationLockMode::AccessExclusive));
    assert!(!session.peer_acquires("public.moved", RelationLockMode::AccessShare));
    assert!(!session.peer_acquires("public.outer_view", RelationLockMode::AccessShare));
}

#[test]
fn a_removed_edge_does_not_hide_another_root_that_still_requires_the_view() {
    let session = Session::new(|views| {
        views.insert(name("dependent"), view(1, "other"));
    });
    assert_eq!(
        lock_dependent_views(
            &session,
            &session,
            &["public.root".into(), "public.other".into()]
        )
        .unwrap(),
        ["public.dependent", "public.outer_view"]
    );
    assert!(!session.peer_acquires("public.dependent", RelationLockMode::AccessShare));
}
