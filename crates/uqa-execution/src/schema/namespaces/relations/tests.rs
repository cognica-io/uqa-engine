//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};
use uqa_sql::catalog::{
    resolution::{candidates::SearchPathRead, creation::CreationRelationNames},
    roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition, RoleMembership, RoleMembershipKey,
    },
    security::{
        database::DatabaseSecurity,
        database_inquiry::DatabaseSecurityRead,
        schema_inquiry::{GraphNamespaceRead, SchemaRegistryRead},
        SchemaSecurity,
    },
};
use uqa_storage::StorageBackendError;

struct Fixture {
    user: String,
    path: Vec<String>,
    schemas: RefCell<BTreeMap<String, SchemaSecurity>>,
    roles: BTreeMap<String, RoleDefinition>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    database: DatabaseSecurity,
    events: RefCell<Vec<&'static str>>,
    allocated: Cell<bool>,
    deferred: bool,
    publish_on_fence: bool,
    fail: Option<&'static str>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            user: "uqa".into(),
            path: vec!["tenant".into()],
            schemas: RefCell::new(BTreeMap::new()),
            roles: BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
            memberships: BTreeMap::new(),
            database: DatabaseSecurity::bootstrap(),
            events: RefCell::new(Vec::new()),
            allocated: Cell::new(false),
            deferred: true,
            publish_on_fence: false,
            fail: None,
        }
    }
    fn context(&self) -> RelationCreationContext<'_> {
        RelationCreationContext {
            names: self,
            roles: self,
            schemas: self,
            database: self,
            state: self,
            relations: self,
            runtime: self,
        }
    }
    fn refresh(&self, kind: &'static str) -> StorageBackendResult<()> {
        self.events.borrow_mut().push(kind);
        if self.fail == Some(kind) {
            Err(StorageBackendError::Other(format!("{kind} failed")))
        } else {
            Ok(())
        }
    }
}
struct EmptyNames;
impl CreationRelationNames for EmptyNames {
    fn contains(&self, _: &RelationIdentity) -> bool {
        false
    }
}
impl GraphNamespaceRead for EmptyNames {
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(std::iter::empty())
    }
    fn contains(&self, _: &str) -> bool {
        false
    }
}
impl RoleReferenceNames for Fixture {
    fn current_user_name(&self) -> String {
        self.events.borrow_mut().push("user");
        self.user.clone()
    }
    fn session_user_name(&self) -> String {
        panic!("creation authorization uses current role")
    }
}
impl RoleCatalogGuards for Fixture {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}
impl RelationCandidateState for Fixture {
    fn temporary_schema_name(&self) -> String {
        "pg_temp_42".into()
    }
    fn search_path(&self) -> SearchPathRead<'_> {
        Box::new(&self.path)
    }
}
impl SchemaPrivilegeCatalog for Fixture {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError> {
        panic!("creation preserves its own refresh envelope")
    }
    fn schemas(&self) -> SchemaRegistryRead<'_> {
        Box::new(self.schemas.borrow())
    }
    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_> {
        Box::new(EmptyNames)
    }
    fn temporary_namespace_allocated(&self) -> bool {
        self.allocated.get()
    }
    fn temporary_schema_name(&self) -> String {
        RelationCandidateState::temporary_schema_name(self)
    }
}
impl DatabasePrivilegeCatalog for Fixture {
    fn refresh_privilege_catalog(&self) -> Result<(), SQLError> {
        panic!("direct privilege checks do not refresh")
    }
    fn security(&self) -> DatabaseSecurityRead<'_> {
        Box::new(&self.database)
    }
}
impl CreationRelationGuards for Fixture {
    fn tables(&self) -> Box<dyn CreationRelationNames + '_> {
        self.events.borrow_mut().push("table_names");
        Box::new(EmptyNames)
    }
    fn views(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(EmptyNames)
    }
    fn sequences(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(EmptyNames)
    }
    fn foreign_tables(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(EmptyNames)
    }
    fn indexes(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(EmptyNames)
    }
}
impl RelationCreationRuntime for Fixture {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        self.refresh("catalog")
    }
    fn synchronize_table_catalog(&self) -> StorageBackendResult<()> {
        self.refresh("tables")
    }
    fn synchronize_table_data(&self) -> StorageBackendResult<()> {
        self.refresh("data")
    }
    fn backend_transaction_is_deferred(&self) -> bool {
        self.events.borrow_mut().push("deferred");
        self.deferred
    }
    fn fence_catalog_writer_and_refresh_snapshot(&self) -> Result<(), SQLError> {
        self.events.borrow_mut().push("fence");
        if self.fail == Some("fence") {
            return Err(SQLError::Internal("writer fence failed".into()));
        }
        if self.publish_on_fence {
            self.schemas
                .borrow_mut()
                .insert("tenant".into(), SchemaSecurity::legacy("tenant"));
        }
        Ok(())
    }
    fn allocate_temporary_namespace(&self) {
        self.events.borrow_mut().push("allocate");
        self.allocated.set(true);
    }
}

#[test]
fn deferred_creation_retries_after_writer_fence_using_the_original_role() {
    let mut fixture = Fixture::new();
    fixture.publish_on_fence = true;
    assert_eq!(
        fixture.context().resolve_persistent_name("docs").unwrap(),
        "tenant.docs"
    );
    assert_eq!(
        &*fixture.events.borrow(),
        &["user", "catalog", "deferred", "fence", "catalog"]
    );
    assert!(!fixture.allocated.get());
}

#[test]
fn missing_creation_namespace_retries_only_once_and_preserves_fence_errors() {
    let mut fixture = Fixture::new();
    let error = fixture
        .context()
        .resolve_persistent_name("tenant.docs")
        .unwrap_err();
    assert!(
        matches!(error,SQLError::Routine {sqlstate,message} if sqlstate=="3F000" && message=="schema \"tenant\" does not exist")
    );
    assert_eq!(
        &*fixture.events.borrow(),
        &["user", "catalog", "deferred", "fence", "catalog"]
    );
    fixture.events.borrow_mut().clear();
    fixture.fail = Some("fence");
    let error = fixture
        .context()
        .resolve_persistent_name("docs")
        .unwrap_err();
    assert!(matches!(error,SQLError::Internal(message) if message=="writer fence failed"));
    assert_eq!(
        &*fixture.events.borrow(),
        &["user", "catalog", "deferred", "fence"]
    );
    fixture.events.borrow_mut().clear();
    fixture.fail = None;
    fixture.deferred = false;
    assert!(
        matches!(fixture.context().resolve_persistent_name("docs"),Err(SQLError::Routine {sqlstate,..}) if sqlstate=="3F000")
    );
    assert_eq!(&*fixture.events.borrow(), &["user", "catalog", "deferred"]);
}

#[test]
fn creation_refresh_failures_precede_namespace_reads_and_keep_call_specific_diagnostics() {
    let mut fixture = Fixture::new();
    fixture.fail = Some("data");
    let error = fixture
        .context()
        .resolve_index_table("tenant.docs")
        .unwrap_err();
    assert!(matches!(error,SQLError::Internal(message) if message=="load table data: data failed"));
    assert_eq!(&*fixture.events.borrow(), &["tables", "data"]);
    fixture.events.borrow_mut().clear();
    fixture.fail = Some("catalog");
    assert_eq!(
        fixture.context().api_name("docs").unwrap_err(),
        "refresh schema catalog: catalog failed"
    );
    assert_eq!(&*fixture.events.borrow(), &["catalog"]);
    fixture.events.borrow_mut().clear();
    fixture.fail = None;
    let error = fixture
        .context()
        .resolve_index_table("missing.docs")
        .unwrap_err();
    assert!(matches!(error,SQLError::Routine {sqlstate,..} if sqlstate=="3F000"));
    assert_eq!(&*fixture.events.borrow(), &["tables", "data", "catalog"]);
}

#[test]
fn temporary_creation_authorizes_before_syntax_and_allocates_only_after_validation() {
    let mut fixture = Fixture::new();
    fixture.user = "guest".into();
    fixture.database.acl = Some(Vec::new());
    assert!(
        matches!(fixture.context().temporary_name("bad.name.extra"),Err(SQLError::Routine {sqlstate,..}) if sqlstate=="42501")
    );
    assert!(!fixture.allocated.get());
    fixture.user = "uqa".into();
    assert!(matches!(
        fixture.context().temporary_name("bad.name.extra"),
        Err(SQLError::Unsupported(_))
    ));
    assert!(matches!(
        fixture.context().temporary_name("public.docs"),
        Err(SQLError::Unsupported(_))
    ));
    assert!(!fixture.allocated.get());
    assert_eq!(
        fixture.context().temporary_name("pg_temp.docs").unwrap(),
        "pg_temp_42.docs"
    );
    assert!(fixture.allocated.get());
    assert_eq!(fixture.events.borrow().last(), Some(&"allocate"));
}
