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
use uqa_sql::{
    catalog::security::{
        schema::SchemaAclPrivilege,
        schema_inquiry::{
            GraphNamespaceRead, SchemaPrivilegeCatalog, SchemaPrivilegeInquiry, SchemaRegistryRead,
        },
        SchemaSecurity,
    },
    SQLError,
};

struct GraphNamesGuard<'a>(MappedRwLockReadGuard<'a, BTreeMap<String, Arc<GraphStoreHandle>>>);
impl GraphNamespaceRead for GraphNamesGuard<'_> {
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.0.keys().map(String::as_str))
    }
    fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
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
        Box::new(GraphNamesGuard(self.durable.graphs.read()))
    }
    fn temporary_namespace_allocated(&self) -> bool {
        self.temporary_namespace_allocated()
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
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> bool {
        self.schema_privilege_inquiry()
            .schema_has_privilege_for_role(schema, role, privilege)
    }
    pub(crate) fn schema_security_for_privilege(&self, schema: &str) -> Option<SchemaSecurity> {
        self.schema_privilege_inquiry()
            .schema_security_for_privilege(schema)
    }
    pub(crate) fn require_schema_privilege(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        self.schema_privilege_inquiry()
            .require_schema_privilege(schema, role, privilege)
    }
}
