//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind domain declaration consumers to current namespace, expression, and publication state.

use crate::Engine;
use uqa_execution::schema::domains::{DomainCreationContext, DomainDeclarationBinding};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{ast::CreateDomain, catalog::domain::StoredDomain, SQLError};

impl Engine {
    pub(crate) fn domain_creation_context(&self) -> DomainCreationContext<'_> {
        DomainCreationContext {
            creation: self.relation_creation_context(),
            identities: self.catalog_identity_reservation_context(),
            writer: self,
            bindings: self,
            allocate_identity: || {
                crate::new_nonzero_catalog_identity("domain", "object identity")
                    .map_err(|error| SQLError::Internal(error.to_string()))
            },
            publication: self,
            enums: self,
            changes: self,
        }
    }
}
impl DomainDeclarationBinding for Engine {
    fn bind_domain_declaration(&self, definition: &mut CreateDomain) -> Result<(), SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        let binding = uqa_execution::query::binding::binding_context(&scope)?;
        let names = self
            .schema_publication_context()
            .constraint_names()
            .automatic_names(&definition.name)?;
        uqa_sql::schema::domains::prepare_domain_definition(
            &uqa_sql::schema::SchemaBindingContext {
                catalog: self,
                binding: &binding,
            },
            self,
            definition,
            &names,
        )
    }
}
use std::collections::BTreeMap;
use uqa_execution::schema::domains::dependencies::{
    DomainDependencyCatalog, DomainDependencyContext, DomainRegistryPublication,
};

impl Engine {
    pub(crate) fn domain_dependency_context(&self) -> DomainDependencyContext<'_> {
        DomainDependencyContext {
            catalog: self,
            publication: self,
            enums: self,
            changes: self,
        }
    }
}
impl DomainDependencyCatalog for Engine {
    fn domain_definitions(&self) -> BTreeMap<String, StoredDomain> {
        self.durable.domains.read().clone()
    }
}
impl DomainRegistryPublication for Engine {
    fn domain_registry(&self) -> uqa_execution::catalog::domain::DomainRegistryRead<'_> {
        Box::new(self.durable.domains.read())
    }
    fn domain_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn publish_domain_definitions(&self, registry: BTreeMap<String, StoredDomain>) {
        *self.durable.domains.write() = registry;
    }
}

use uqa_execution::schema::domains::removal::{DomainDropNotices, DomainRemovalContext};
use uqa_sql::catalog::security::BoundSchemaSecurity;
use uqa_sql::schema::domains::removal::{
    TypeObjectAuthority, TypeObjectBinding, TypeObjectCatalog,
};

impl Engine {
    pub(crate) fn domain_removal_context(&self) -> DomainRemovalContext<'_> {
        DomainRemovalContext {
            refresh: self,
            binding: TypeObjectBinding {
                catalog: self,
                authority: self,
                session: self,
            },
            deletion: self,
            notices: self,
        }
    }
}
impl TypeObjectCatalog for Engine {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        self.schema_security_for_privilege(name)
    }
    fn resolve_drop_type_oid(&self, name: &str) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_type_object_oid(&self.catalog_execution(), name)
    }
    fn format_drop_type(&self, oid: i64) -> Result<Option<String>, String> {
        uqa_execution::catalog::projection::format_type_object(&self.catalog_execution(), oid)
    }
    fn enum_by_type_oid(&self, oid: u32) -> Option<uqa_sql::catalog::enum_type::StoredEnum> {
        self.durable
            .enums
            .read()
            .values()
            .find(|definition| definition.oid == oid || definition.array_oid == oid)
            .cloned()
    }
    fn user_array_element(&self, oid: u32) -> Option<u32> {
        self.durable.domains.read().values().find_map(|domain| {
            (uqa_sql::catalog::type_metadata::pg_type_array_oid(&domain.column_type())
                == i64::from(oid))
            .then_some(domain.oid)
        })
    }
    fn row_type_relation(
        &self,
        oid: u32,
    ) -> Option<uqa_sql::schema::domains::removal::RowTypeRelation> {
        uqa_execution::catalog::projection::row_type_relation(&self.catalog_execution(), oid)
            .ok()
            .flatten()
    }
}
impl TypeObjectAuthority for Engine {
    fn schema_usage(&self, schema: &str, role: &RoleReference) -> bool {
        self.schema_has_privilege_for_role(
            schema,
            role,
            uqa_sql::catalog::security::schema::SchemaAclPrivilege::Usage,
        )
    }
    fn current_user_has_role_privileges(
        &self,
        role: &dyn uqa_sql::catalog::roles::identity::RoleSubject,
    ) -> bool {
        Engine::current_user_has_role_privileges(self, role)
    }
}
impl DomainDropNotices for Engine {
    fn domain_drop_notice(&self, notice: uqa_sql::SQLNotice) {
        self.push_sql_notice(notice);
    }
}
