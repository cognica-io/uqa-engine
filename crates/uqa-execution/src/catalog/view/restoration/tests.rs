//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::{
        foreign::StoredForeignTable,
        view::{ViewIdentityAllocation, ViewRegistryRead, ViewRegistryState, ViewRegistryWrite},
    },
    schema::view_creation::context::ViewPlanBinding,
};
use context::*;
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};
use uqa_graph::GraphStoreHandle;
use uqa_sql::{
    ast::{ColumnType, RelationPersistence},
    catalog::roles::{
        guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition,
    },
    plan::{QueryPlan, UnifiedPlan},
    RowSchema, SQLError, SQLParam,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    Binding,
    Schema,
    Persistence,
}
struct Fixture {
    registry: RefCell<BTreeMap<RelationIdentity, StoredView>>,
    rows: Vec<ViewRow>,
    saved: RefCell<Vec<ViewRow>>,
    failure: Failure,
    bindings: Cell<usize>,
    schemas: Cell<usize>,
    identities: Cell<u8>,
    writes: Cell<usize>,
    roles: BTreeMap<String, RoleDefinition>,
    foreign: BTreeMap<RelationIdentity, StoredForeignTable>,
    graphs: BTreeMap<String, Arc<GraphStoreHandle>>,
}
fn view(object_id: u8, persistence: RelationPersistence) -> StoredView {
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS value").unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    StoredView {
        object_id: [object_id; 16],
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["value".into()]),
        persistence,
        options: Vec::new(),
        kind: StoredViewKind::View,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}
impl Fixture {
    fn new(failure: Failure) -> Self {
        let rows = ["first", "second"]
            .into_iter()
            .map(|name| ViewRow {
                relation: RelationIdentity::new("public", name),
                role_owner: "owner".into(),
                acl: None,
                column_acls: BTreeMap::new(),
                definition_json: serde_json::to_string(
                    &view(0, RelationPersistence::Permanent).query,
                )
                .unwrap(),
            })
            .collect();
        Self {
            registry: RefCell::new(BTreeMap::from([
                (
                    RelationIdentity::new("public", "previous"),
                    view(8, RelationPersistence::Permanent),
                ),
                (
                    RelationIdentity::new("pg_temp_7", "session_view"),
                    view(9, RelationPersistence::Temporary),
                ),
            ])),
            rows,
            saved: RefCell::new(Vec::new()),
            failure,
            bindings: Cell::new(0),
            schemas: Cell::new(0),
            identities: Cell::new(0),
            writes: Cell::new(0),
            roles: BTreeMap::from([(
                "owner".into(),
                RoleDefinition {
                    oid: 42,
                    name: "owner".into(),
                    attributes: BTreeSet::new(),
                    connection_limit: -1,
                },
            )]),
            foreign: BTreeMap::new(),
            graphs: BTreeMap::new(),
        }
    }
    fn context(&self) -> ViewRestoreContext<'_> {
        ViewRestoreContext {
            registry: self,
            namespace: self,
            roles: self,
            identities: self,
            bindings: self,
            schemas: self,
            sequences: self,
        }
    }
    fn snapshot(
        &self,
    ) -> Vec<(
        RelationIdentity,
        serde_json::Value,
        uqa_sql::catalog::security::TableSecurity,
    )> {
        self.registry
            .borrow()
            .iter()
            .map(|(name, view)| {
                (
                    name.clone(),
                    serde_json::to_value(view).unwrap(),
                    view.security(),
                )
            })
            .collect()
    }
    fn assert_provisional_namespace(&self) {
        assert!(
            self.registry.try_borrow_mut().is_ok(),
            "analysis runs after releasing the publication guard"
        );
        let views = self.registry.borrow();
        assert_eq!(views.len(), 3);
        assert!(views.contains_key(&RelationIdentity::new("public", "first")));
        assert!(views.contains_key(&RelationIdentity::new("public", "second")));
        assert!(views.contains_key(&RelationIdentity::new("pg_temp_7", "session_view")));
    }
}
impl ViewRowsStorage for Fixture {
    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        Ok(self.rows.clone())
    }
    fn save_view(&self, row: &ViewRow) -> StorageBackendResult<()> {
        if self.failure == Failure::Persistence && !self.saved.borrow().is_empty() {
            return Err(StorageBackendError::Other(
                "injected persistence failure".into(),
            ));
        }
        self.saved.borrow_mut().push(row.clone());
        Ok(())
    }
}
impl ViewRegistryState for Fixture {
    fn views_read(&self) -> ViewRegistryRead<'_> {
        Box::new(self.registry.borrow())
    }
    fn views_write(&self) -> ViewRegistryWrite<'_> {
        self.writes.set(self.writes.get() + 1);
        Box::new(self.registry.borrow_mut())
    }
}
impl ViewIdentityAllocation for Fixture {
    fn allocate_identity(&self) -> StorageBackendResult<[u8; 16]> {
        let next = self.identities.get() + 1;
        self.identities.set(next);
        Ok([next; 16])
    }
}
struct NoTables;
impl ViewRestoreTableNames for NoTables {
    fn names(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(std::iter::empty())
    }
}
impl ViewRestoreNamespace for Fixture {
    fn tables(&self) -> Box<dyn ViewRestoreTableNames + '_> {
        Box::new(NoTables)
    }
    fn foreign_tables(&self) -> ViewRestoreForeignRead<'_> {
        Box::new(&self.foreign)
    }
    fn graphs(&self) -> ViewRestoreGraphsRead<'_> {
        Box::new(&self.graphs)
    }
}
impl RoleCatalogGuards for Fixture {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        panic!("view metadata validation does not read memberships");
    }
}
impl ViewPlanBinding for Fixture {
    fn bind_relations(&self, _: &mut QueryPlan) -> Result<bool, SQLError> {
        panic!("restoration uses its explicit persisted namespace");
    }
    fn bind_routines(
        &self,
        plan: &mut QueryPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        assert!(params.is_empty());
        self.assert_provisional_namespace();
        assert!(plan.relations_bound);
        if self.bindings.get() == 1 {
            assert!(
                !self.registry.borrow()[&RelationIdentity::new("public", "first")]
                    .query
                    .relations_bound,
                "the first bound definition is published before binding the next view"
            );
        }
        // Mark the definition so the next binding observes its individual publication.
        plan.relations_bound = false;
        self.bindings.set(self.bindings.get() + 1);
        if self.failure == Failure::Binding && self.bindings.get() == 2 {
            return Err(SQLError::Internal("injected binding failure".into()));
        }
        Ok(RowSchema::with_types(
            vec!["value".into()],
            vec![Some(ColumnType::Integer)],
        ))
    }
}
impl ViewRestoreSchemas for Fixture {
    fn stored_schema(&self, _: &StoredView) -> Result<RowSchema, SQLError> {
        self.assert_provisional_namespace();
        assert_eq!(self.bindings.get(), 2);
        self.schemas.set(self.schemas.get() + 1);
        if self.failure == Failure::Schema && self.schemas.get() == 2 {
            return Err(SQLError::Internal("injected schema failure".into()));
        }
        Ok(RowSchema::with_types(
            vec!["value".into()],
            vec![Some(ColumnType::Integer)],
        ))
    }
}
impl ViewRestoreSequences for Fixture {
    fn resolve_loaded(&self, _: &str) -> StorageBackendResult<String> {
        panic!("query fixture has no sequence reference");
    }
}

#[test]
fn view_migration_publishes_the_complete_namespace_before_binding_and_keeps_temporary_views() {
    let fixture = Fixture::new(Failure::None);
    restore_views_from_catalog(&fixture.context(), &fixture, true).unwrap();
    assert_eq!(fixture.bindings.get(), 2);
    assert_eq!(fixture.schemas.get(), 2);
    assert_eq!(fixture.identities.get(), 2);
    assert_eq!(fixture.saved.borrow().len(), 2);
    fixture.assert_provisional_namespace();
    let views = fixture.registry.borrow();
    assert_eq!(
        views[&RelationIdentity::new("pg_temp_7", "session_view")].object_id,
        [9; 16]
    );
    for name in ["first", "second"] {
        let restored = &views[&RelationIdentity::new("public", name)];
        assert_ne!(restored.object_id, [0; 16]);
        assert_eq!(restored.output_columns, Some(vec!["value".into()]));
        assert_eq!(restored.role_owner, "owner");
    }
}

#[test]
fn view_migration_failures_restore_the_previous_registry_including_security_and_temporary_views() {
    for failure in [Failure::Binding, Failure::Schema, Failure::Persistence] {
        let fixture = Fixture::new(failure);
        let before = fixture.snapshot();
        let error = restore_views_from_catalog(&fixture.context(), &fixture, true).unwrap_err();
        assert!(error.to_string().contains("injected"));
        assert_eq!(fixture.snapshot(), before);
        assert!(fixture.registry.try_borrow_mut().is_ok());
    }
}

#[test]
fn load_only_view_restore_rejects_migration_before_registry_or_storage_publication() {
    let fixture = Fixture::new(Failure::None);
    let before = fixture.snapshot();
    let error = restore_views_from_catalog(&fixture.context(), &fixture, false).unwrap_err();
    assert!(error
        .to_string()
        .contains("requires an initial-open metadata migration"));
    assert_eq!(fixture.identities.get(), 2);
    assert_eq!(fixture.writes.get(), 0);
    assert_eq!(fixture.bindings.get(), 0);
    assert!(fixture.saved.borrow().is_empty());
    assert_eq!(fixture.snapshot(), before);
}
