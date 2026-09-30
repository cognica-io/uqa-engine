//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind schema privilege rules to live namespace registries and session identity.

use crate::Engine;
use parking_lot::MappedRwLockReadGuard;
use std::{collections::BTreeMap, sync::Arc};
use uqa_graph::GraphStoreHandle;
use uqa_sql::catalog::graph_oids::GraphCatalogOids;
use uqa_sql::catalog::roles::identity::RoleSubject;
use uqa_sql::{
    catalog::security::{
        schema::SchemaAclPrivilege,
        schema_inquiry::{
            GraphNamespaceRead, SchemaPrivilegeCatalog, SchemaPrivilegeInquiry, SchemaRegistryRead,
        },
        BoundSchemaSecurity,
    },
    SQLError,
};

struct GraphNamesGuard<'a> {
    graphs: MappedRwLockReadGuard<'a, BTreeMap<String, Arc<GraphStoreHandle>>>,
    oids: MappedRwLockReadGuard<'a, BTreeMap<String, GraphCatalogOids>>,
}
impl GraphNamespaceRead for GraphNamesGuard<'_> {
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.graphs.keys().map(String::as_str))
    }
    fn contains(&self, name: &str) -> bool {
        self.graphs.contains_key(name)
    }
    fn namespace_oid(&self, name: &str) -> i64 {
        self.oids.get(name).map_or_else(
            || uqa_sql::catalog::oids::schema_oid(name),
            |oids| i64::from(oids.namespace),
        )
    }
}

impl SchemaPrivilegeCatalog for Engine {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load schemas for privilege inquiry: {error}"))
        })
    }
    fn schemas(&self) -> SchemaRegistryRead<'_> {
        Box::new(self.durable.schemas.read())
    }
    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_> {
        Box::new(GraphNamesGuard {
            graphs: self.durable.graphs.read(),
            oids: self.durable.graph_catalog_oids.read(),
        })
    }
    fn temporary_namespace_oids(
        &self,
    ) -> Option<uqa_sql::catalog::temporary_namespace::TemporaryNamespaceOids> {
        self.temporary_namespace_oids()
    }
    fn temporary_schema_name(&self) -> String {
        self.temporary_schema_name()
    }
}

impl Engine {
    pub(crate) fn schema_privilege_inquiry(&self) -> SchemaPrivilegeInquiry<'_> {
        SchemaPrivilegeInquiry {
            catalog: self,
            names: self,
            roles: self,
        }
    }
    pub(crate) fn schema_has_privilege_for_role(
        &self,
        schema: &str,
        role: &(impl RoleSubject + ?Sized),
        privilege: SchemaAclPrivilege,
    ) -> bool {
        self.schema_privilege_inquiry()
            .schema_has_privilege_for_role(schema, role, privilege)
    }
    pub(crate) fn schema_security_for_privilege(
        &self,
        schema: &str,
    ) -> Option<BoundSchemaSecurity> {
        self.schema_privilege_inquiry()
            .schema_security_for_privilege(schema)
    }
    pub(crate) fn require_schema_privilege(
        &self,
        schema: &str,
        role: &(impl RoleSubject + ?Sized),
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        self.schema_privilege_inquiry()
            .require_schema_privilege(schema, role, privilege)
    }
}

use uqa_core::RelationIdentity;
use uqa_execution::schema::namespaces::relations::{
    RelationCreationContext, RelationCreationRuntime,
};
use uqa_sql::catalog::resolution::creation::{CreationRelationGuards, CreationRelationNames};
use uqa_storage::StorageBackendResult;

struct CreationNamesGuard<G>(G);
impl<T, G: std::ops::Deref<Target = BTreeMap<RelationIdentity, T>>> CreationRelationNames
    for CreationNamesGuard<G>
{
    fn contains(&self, relation: &RelationIdentity) -> bool {
        self.0.contains_key(relation)
    }
}
impl CreationRelationGuards for Engine {
    fn named_type_exists(&self, identity: &RelationIdentity) -> bool {
        uqa_execution::catalog::projection::named_type_exists(
            self.durable.domains.read().values(),
            self.durable.enums.read().values(),
            identity,
        )
    }
    fn tables(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(CreationNamesGuard(self.storage.tables.read()))
    }
    fn views(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(CreationNamesGuard(self.durable.views.read()))
    }
    fn sequences(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(CreationNamesGuard(self.durable.sequences.read()))
    }
    fn foreign_tables(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(CreationNamesGuard(self.durable.foreign_tables.read()))
    }
    fn indexes(&self) -> Box<dyn CreationRelationNames + '_> {
        Box::new(CreationNamesGuard(self.durable.catalog_indexes.read()))
    }
}
impl RelationCreationRuntime for Engine {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        Engine::synchronize_catalog_registries(self)
    }
    fn synchronize_table_catalog(&self) -> StorageBackendResult<()> {
        Engine::synchronize_table_catalog(self)
    }
    fn synchronize_table_data(&self) -> StorageBackendResult<()> {
        Engine::synchronize_table_data(self)
    }
    fn backend_transaction_is_deferred(&self) -> bool {
        Engine::backend_transaction_is_deferred(self)
    }
    fn fence_catalog_writer_and_refresh_snapshot(&self) -> Result<(), SQLError> {
        Engine::fence_catalog_writer_and_refresh_snapshot(self)
    }
    fn create_temporary_namespace(&self) -> Result<(), SQLError> {
        let oids = self
            .catalog_identity_reservation_context()
            .allocator(uqa_execution::catalog::identity::allocate_catalog_object_id)
            .allocate_temporary_namespace_oids(|oid| {
                Ok(uqa_execution::schema::namespaces::identity::namespace_oid_in_use(self, oid))
            })?;
        self.session.state.write().temporary_namespace = Some(oids);
        Ok(())
    }
}
impl Engine {
    pub(crate) fn relation_creation_context(&self) -> RelationCreationContext<'_> {
        RelationCreationContext {
            names: self,
            roles: self,
            locks: self,
            schemas: self,
            database: self,
            state: self,
            relations: self,
            runtime: self,
        }
    }
}
