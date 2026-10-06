//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    routines::catalog::{RoutineMutationContext, RoutineRegistryPublication, RoutineRegistryState},
    row_locks::{
        shared_objects::{SharedCatalogLock, SharedObjectLockSession},
        RelationLockMode, RowLockManager, ScopedRelationLock,
    },
    schema::namespaces::{NamespaceCatalogChanges, SchemaStatementWriter},
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::{ColumnDef, ColumnType},
    catalog::{
        roles::{
            guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
            RoleDefinition, RoleMembership, RoleMembershipKey, RoleReference, RoleReferenceNames,
        },
        security::{ownership::RelationOwnerSchemas, BoundSchemaSecurity},
    },
    routines::{declaration::RoutineTypeCatalog, lifecycle::names::RoutineNameCatalog},
};

pub(super) struct Fixture {
    pub registry: RefCell<RoutineRegistry>,
    pub builtins: RefCell<uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities>,
    pub persisted: RefCell<Vec<String>>,
    pub on_writer: RefCell<Option<RoutineRegistry>>,
    pub fail_persist: Cell<bool>,
    pub notices: RefCell<Vec<uqa_sql::SQLNotice>>,
    pub current: RefCell<String>,
    pub denied_schema: Cell<bool>,
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    locks: RowLockManager,
    cancel: uqa_core::CancellationToken,
}
impl Fixture {
    pub fn new() -> Self {
        let mut reader = RoleDefinition::bootstrap();
        reader.name = "reader".into();
        reader.oid = 20_000;
        reader.object_id = [2; 16];
        reader
            .attributes
            .remove(&uqa_sql::ast::RoleAttribute::Superuser);
        Self {
            registry: RefCell::new(BTreeMap::new()),
            builtins: RefCell::new(BTreeMap::new()),
            persisted: RefCell::new(Vec::new()),
            on_writer: RefCell::new(None),
            fail_persist: Cell::new(false),
            notices: RefCell::new(Vec::new()),
            current: RefCell::new("uqa".into()),
            denied_schema: Cell::new(false),
            roles: RefCell::new(BTreeMap::from([
                ("uqa".into(), RoleDefinition::bootstrap()),
                ("reader".into(), reader),
            ])),
            memberships: RefCell::new(BTreeMap::new()),
            locks: RowLockManager::new(),
            cancel: uqa_core::CancellationToken::new(),
        }
    }
    pub fn context(&self) -> RoutinePrivilegeContext<'_> {
        RoutinePrivilegeContext {
            snapshot: crate::catalog::test_support::empty_catalog(),
            builtin_security: self,
            storage: None,
            catalog: RoutineMutationContext {
                writer: self,
                names: self,
                roles: self,
                registry: self,
                publication: self,
                changes: self,
            },
            locks: self,
            schemas: self,
            types: self,
            role_names: self,
            notices: self,
        }
    }
    pub fn add(&self, name: &str, oid: u32, procedure: bool) {
        let sql = if procedure {
            format!("CREATE PROCEDURE {name}() LANGUAGE SQL AS 'SELECT 1'")
        } else {
            format!("CREATE FUNCTION {name}() RETURNS integer LANGUAGE SQL AS 'SELECT 1'")
        };
        let uqa_sql::Statement::CreateFunction(mut def) = uqa_sql::compile(&sql).unwrap().remove(0)
        else {
            panic!("routine");
        };
        def.owner = Some(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP);
        def.object_id = Some([oid as u8; 16]);
        def.catalog_oid = Some(oid);
        self.registry.borrow_mut().insert(
            name.into(),
            vec![std::sync::Arc::new(
                uqa_sql::routines::SQLUserFunction::new(
                    *def,
                    uqa_sql::routines::RoutineBody::Source,
                ),
            )],
        );
    }
    pub fn grant(&self, sql: &str) -> Result<(), SQLError> {
        let uqa_sql::Statement::GrantRoutine(stmt) = uqa_sql::compile(sql)?.remove(0) else {
            panic!("grant");
        };
        grant_sql_routine(&self.context(), &stmt)
    }
    pub fn allowed(&self, name: &str) -> bool {
        let registry = self.registry.borrow();
        let def = &registry[name][0].def;
        let reader = self.roles.borrow()["reader"].identity();
        uqa_sql::routines::security::routine_privilege_allowed(
            &def.owner.unwrap(),
            def.execute_acl.as_deref(),
            false,
            false,
            |role| *role == reader,
        )
    }
    fn released(&self) {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
        assert!(self.registry.try_borrow_mut().is_ok());
    }
}
impl RoutineRegistryState for Fixture {
    fn routine_snapshot(&self) -> RoutineRegistry {
        self.registry.borrow().clone()
    }
    fn routines_write(&self) -> RoutineRegistryWrite<'_> {
        Box::new(self.registry.borrow_mut())
    }
}
impl RoutineRegistryPublication for Fixture {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError> {
        if self.fail_persist.get() {
            return Err(SQLError::Internal("catalog write failed".into()));
        }
        self.persisted.borrow_mut().push(
            serde_json::to_string(
                &registry
                    .iter()
                    .map(|(name, routines)| {
                        (
                            name,
                            routines
                                .iter()
                                .map(|routine| &routine.def)
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            )
            .unwrap(),
        );
        Ok(())
    }
}
impl SchemaStatementWriter for Fixture {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.released();
        if let Some(registry) = self.on_writer.borrow_mut().take() {
            *self.registry.borrow_mut() = registry;
        }
        Ok(())
    }
}
impl NamespaceCatalogChanges for Fixture {
    fn catalog_registry_changed(&self) {
        self.released();
    }
}
impl RoleCatalogGuards for Fixture {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(self.memberships.borrow())
    }
}
impl RoleReferenceNames for Fixture {
    fn current_role(&self) -> RoleReference {
        self.current.borrow().clone().into()
    }
    fn session_role(&self) -> RoleReference {
        "uqa".into()
    }
    fn outer_role(&self) -> RoleReference {
        RoleReferenceNames::current_role(self)
    }
}
impl RoutinePrivilegeNotices for Fixture {
    fn routine_privilege_notice(&self, notice: uqa_sql::SQLNotice) {
        self.notices.borrow_mut().push(notice);
    }
}
impl RoutineNameCatalog for Fixture {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        ["app", "empty", "public", "pg_catalog"]
            .contains(&name)
            .then(|| BoundSchemaSecurity::bootstrap(name))
    }
    fn current_role(&self) -> RoleReference {
        RoleReferenceNames::current_role(self)
    }
    fn search_path(&self) -> Vec<String> {
        vec!["app".into()]
    }
    fn require_schema_usage(&self, name: &str, _: &RoleReference) -> Result<(), SQLError> {
        if self.denied_schema.get() && name == "app" {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied for schema app".into(),
            })
        } else {
            Ok(())
        }
    }
    fn schema_has_usage(&self, name: &str, role: &RoleReference) -> bool {
        self.require_schema_usage(name, role).is_ok()
    }
    fn routine_type_display(&self, name: &str) -> String {
        name.into()
    }
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError> {
        Ok(oid.to_string())
    }
}
impl RelationOwnerSchemas for Fixture {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        RoutineNameCatalog::schema_security(self, name)
    }
}
impl RoutineTypeCatalog for Fixture {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.sql_name())
    }
}
impl SharedObjectLockSession for Fixture {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.released();
        self.locks.acquire_scoped_relation(
            1,
            self.locks.shared_catalog_key(target),
            mode,
            (0, 1),
            &self.cancel,
        )
    }
    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        self.released();
        Ok(())
    }
    fn next_catalog_oid(&self) -> Result<u32, SQLError> {
        self.locks.catalog_oids().next_oid(None, || Ok(None))
    }
}

impl uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurityCatalog for Fixture {
    fn builtin_routine_securities(
        &self,
    ) -> uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurityRead<'_> {
        Box::new(self.builtins.borrow())
    }
}
impl crate::catalog::security::builtin_routines::BuiltinRoutineSecurityState for Fixture {
    fn builtin_routine_securities_write(
        &self,
    ) -> Box<
        dyn std::ops::DerefMut<
                Target = uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities,
            > + '_,
    > {
        Box::new(self.builtins.borrow_mut())
    }
}
