//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    security::roles::persistence::RoleCatalogSnapshot,
    sequence::SequenceState,
    services::{
        CatalogExpressionEvaluation, CatalogNamespace, CatalogSession, RelationCounts,
        ViewCatalogCapabilities, ViewCatalogMetadata,
    },
    test_support::{empty_catalog, NoRoutines},
    CatalogReadView, RelationLookupMode, RelationNameResolution,
};
use uqa_sql::{
    ast::{Expr, TriggerEvent},
    catalog::{
        security::{
            sequence_inquiry::{
                SequencePrivilegeResolution, SequenceSecurityCatalog, SequenceSecurityRead,
            },
            system_relations::{SystemRelationSecurityCatalog, SystemRelationSecurityRead},
        },
        session::PreparedStatementMetadata,
    },
};

pub(super) struct Fixture {
    pub retained: SequenceReadSnapshot,
    pub current: RefCell<SequenceReadSnapshot>,
    pub after_read: RefCell<Option<SequenceReadSnapshot>>,
    pub after_resolution: RefCell<Option<SequenceReadSnapshot>>,
    pub reads: Cell<usize>,
    pub refreshes: Cell<usize>,
    catalog: CatalogReadView,
    cache: RegtypeOutputCache,
}

impl Fixture {
    pub fn new() -> Self {
        let mut reader = RoleDefinition::bootstrap();
        reader.name = "reader".into();
        reader.oid = 20_000;
        reader.object_id = [1; 16];
        reader.attributes.clear();
        let relation = RelationIdentity::new("public", "ids");
        let retained = SequenceReadSnapshot {
            sequences: Arc::new(BTreeMap::from([(
                relation.clone(),
                SequenceState::initial(1, 1, SequenceDataType::BigInt),
            )])),
            object_ids: Arc::new(BTreeMap::from([(relation.clone(), [7; 16])])),
            persistence: Arc::new(BTreeMap::from([(
                relation.clone(),
                RelationPersistence::Permanent,
            )])),
            security: Arc::new(BTreeMap::from([(
                relation,
                BoundSequenceSecurity {
                    role_owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                    acl: Some(vec![uqa_core::catalog_role::BoundAclEntry {
                        role: Some(reader.identity()),
                        grantor: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                        privileges: SequencePrivileges {
                            update: true,
                            ..SequencePrivileges::default()
                        },
                        grant_options: SequencePrivileges::default(),
                    }]),
                },
            )])),
            roles: RoleCatalogSnapshot {
                roles: Arc::new(BTreeMap::from([
                    ("uqa".into(), RoleDefinition::bootstrap()),
                    (reader.name.clone(), reader),
                ])),
                memberships: Arc::new(BTreeMap::new()),
            },
        };
        let mut catalog = empty_catalog().snapshot().clone();
        catalog.definitions.sequences = Arc::clone(&retained.sequences);
        catalog.definitions.sequence_object_ids = Arc::clone(&retained.object_ids);
        catalog.definitions.sequence_persistence = Arc::clone(&retained.persistence);
        catalog.definitions.sequence_security = Arc::clone(&retained.security);
        catalog.definitions.roles = Arc::clone(&retained.roles.roles);
        Self {
            current: RefCell::new(retained.clone()),
            retained,
            after_read: RefCell::new(None),
            after_resolution: RefCell::new(None),
            reads: Cell::new(0),
            refreshes: Cell::new(0),
            catalog: CatalogReadView::new(catalog),
            cache: RegtypeOutputCache::default(),
        }
    }
    pub fn context(&self) -> TablePrivilegeContext<'_> {
        TablePrivilegeContext {
            names: self,
            roles: &self.retained,
            sequences: SequencePrivilegeInquiry {
                names: self,
                roles: &self.retained,
                security: self,
                resolution: self,
            },
            snapshots: self,
            registry: self,
            catalog: CatalogContext {
                catalog: &self.catalog,
                session: &Services,
                namespaces: &Services,
                routines: &NoRoutines,
                expressions: &Services,
                counts: &Services,
                views: &Services,
                cache: &self.cache,
            },
        }
    }
}

impl SequenceSnapshotSource for Fixture {
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot> {
        self.reads.set(self.reads.get() + 1);
        let captured = self.current.borrow().clone();
        if let Some(next) = self.after_read.borrow_mut().take() {
            *self.current.borrow_mut() = next;
        }
        Ok(captured)
    }
}
impl RoleReferenceNames for Fixture {
    fn outer_role(&self) -> uqa_sql::catalog::roles::RoleReference {
        self.current_role()
    }
    fn current_role(&self) -> RoleReference {
        RoleReference::Bound(Arc::new(
            RoleBinding::from_definition(&self.retained.roles.roles["reader"]).unwrap(),
        ))
    }
    fn session_role(&self) -> RoleReference {
        self.current_role()
    }
}
impl SequenceSecurityCatalog for Fixture {
    fn security_read(&self) -> SequenceSecurityRead<'_> {
        Box::new(self.retained.security.as_ref())
    }
}
impl SequencePrivilegeResolution for Fixture {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        assert_eq!(reference, "ids");
        if let Some(next) = self.after_resolution.borrow_mut().take() {
            *self.current.borrow_mut() = next;
        }
        Ok(RelationResolution::Found("public.ids".into(), "sequence"))
    }
    fn sequence_privilege_oid(
        &self,
        _: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        panic!("relation privilege inquiry must not use the live sequence OID path")
    }
}
impl SystemRelationSecurityCatalog for Fixture {
    fn system_relation_securities(&self) -> SystemRelationSecurityRead<'_> {
        Box::new(
            self.catalog
                .snapshot()
                .definitions
                .system_relation_security
                .as_ref(),
        )
    }
}
impl TablePrivilegeRegistry for Fixture {
    fn refresh_tables(&self) -> StorageBackendResult<()> {
        self.refreshes.set(self.refreshes.get() + 1);
        Ok(())
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.refreshes.set(self.refreshes.get() + 1);
        Ok(())
    }
    fn tables(&self) -> Box<dyn TablePrivilegeRead + '_> {
        Box::new(EmptyTables)
    }
    fn views(&self) -> PrivilegeViewsRead<'_> {
        Box::new(self.catalog.snapshot().definitions.views.as_ref())
    }
    fn foreign_tables(&self) -> PrivilegeForeignTablesRead<'_> {
        Box::new(self.catalog.snapshot().definitions.foreign_tables.as_ref())
    }
    fn foreign_security(&self) -> PrivilegeForeignSecurityRead<'_> {
        Box::new(
            self.catalog
                .snapshot()
                .definitions
                .foreign_table_security
                .as_ref(),
        )
    }
}
struct EmptyTables;
impl TablePrivilegeRead for EmptyTables {
    fn security_entries(
        &self,
    ) -> Box<dyn Iterator<Item = (RelationIdentity, BoundTableSecurity)> + '_> {
        Box::new(std::iter::empty())
    }
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(std::iter::empty())
    }
    fn get(&self, _: &RelationIdentity) -> Option<&dyn TablePrivilegeState> {
        None
    }
    fn retained(&self, _: &RelationIdentity) -> Option<Arc<dyn TablePrivilegeState>> {
        None
    }
}

struct Services;
impl CatalogSession for Services {
    fn current_role(&self) -> RoleReference {
        "reader".into()
    }
    fn temporary_schema_name(&self) -> String {
        "pg_temp_1".into()
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "reader".into(),
            lookup_mode: RelationLookupMode::Dynamic,
        }
    }
    fn show_variable(&self, _: &str) -> Result<String, SQLError> {
        unreachable!()
    }
    fn runtime_parameter_source(&self, _: &str) -> &'static str {
        unreachable!()
    }
    fn cursors(&self) -> Vec<uqa_sql::catalog::session::CursorMetadata> {
        panic!("unexpected cursor catalog read")
    }
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata> {
        unreachable!()
    }
}
impl CatalogNamespace for Services {
    fn current_schema_names(&self, _: bool) -> Result<Vec<String>, SQLError> {
        unreachable!()
    }
}
impl CatalogExpressionEvaluation for Services {
    fn evaluate(&self, _: &Expr) -> Result<Value, SQLError> {
        unreachable!()
    }
}
impl RelationCounts for Services {
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        unreachable!()
    }
}
impl ViewCatalogCapabilities for Services {
    fn view_updatability(&self, _: &str) -> Result<ViewCatalogMetadata, SQLError> {
        unreachable!()
    }
    fn has_instead_of_trigger(&self, _: &str, _: TriggerEvent) -> Result<bool, SQLError> {
        unreachable!()
    }
}
