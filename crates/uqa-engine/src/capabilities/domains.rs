//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind domain declaration consumers to current namespace, expression, and publication state.

use crate::Engine;
use uqa_execution::schema::domains::{
    DomainCreationContext, DomainDeclarationBinding, DomainPublication,
};
use uqa_sql::{
    ast::CreateDomain, catalog::domain::StoredDomain, schema::domains::DomainCreationCatalog,
    SQLError,
};

impl Engine {
    pub(crate) fn domain_creation_context(&self) -> DomainCreationContext<'_> {
        DomainCreationContext {
            creation: self.relation_creation_context(),
            writer: self,
            catalog: self,
            bindings: self,
            allocate_identity: || {
                crate::new_nonzero_catalog_identity("domain", "object identity")
                    .map_err(|error| SQLError::Internal(error.to_string()))
            },
            session: self,
            publication: self,
        }
    }
}
impl DomainCreationCatalog for Engine {
    fn domain_type_exists(&self, name: &str) -> bool {
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            name,
        )
        .is_some()
    }
    fn domain_table_exists(&self, name: &str) -> Result<bool, SQLError> {
        self.try_table(name)
            .map(|table| table.is_some())
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
}
impl DomainDeclarationBinding for Engine {
    fn bind_domain_declaration(&self, definition: &mut CreateDomain) -> Result<(), SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        let binding = uqa_execution::query::binding::binding_context(&scope)?;
        uqa_sql::schema::domains::prepare_domain_definition(
            &uqa_sql::schema::SchemaBindingContext {
                catalog: self,
                binding: &binding,
            },
            self,
            definition,
        )
    }
}
impl DomainPublication for Engine {
    fn publish_domain(&self, domain: StoredDomain) -> Result<(), SQLError> {
        Engine::publish_domain(self, domain)
    }
}

use crate::TableState;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::foreign::StoredForeignTable;
use uqa_execution::schema::domains::dependencies::{
    DomainCheckRead, DomainColumnRead, DomainDependencyCatalog, DomainDependencyContext,
    DomainForeignRemoval, DomainIndexRemoval, DomainRegistryPublication, DomainTableMetadata,
    DomainTableRemoval, DomainViewDependencies,
};
use uqa_sql::schema::domains::dependencies::DomainTypeCatalog;
use uqa_sql::{ast::ColumnType, catalog::stored_view::StoredView};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};

impl Engine {
    pub(crate) fn domain_dependency_context(&self) -> DomainDependencyContext<'_> {
        DomainDependencyContext {
            types: self,
            catalog: self,
            views: self,
            publication: self,
            tables: self,
            foreign: self,
            indexes: self,
            events: self,
            locks: self,
            changes: self,
        }
    }
}
impl DomainTypeCatalog for Engine {
    fn resolve_domain_type_reference(&self, name: &str) -> Option<ColumnType> {
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            name,
        )
    }
}
impl DomainDependencyCatalog for Engine {
    fn domain_definitions(&self) -> BTreeMap<String, StoredDomain> {
        self.durable.domains.read().clone()
    }
    fn domain_index_rows(&self) -> BTreeMap<RelationIdentity, CatalogIndexRow> {
        self.durable.catalog_indexes.read().clone()
    }
    fn domain_table_schemas(&self) -> Vec<(String, Arc<dyn DomainTableMetadata>)> {
        self.table_entries()
            .into_iter()
            .map(|(name, table)| (name, table as Arc<dyn DomainTableMetadata>))
            .collect()
    }
    fn domain_foreign_tables(&self) -> BTreeMap<RelationIdentity, StoredForeignTable> {
        self.durable.foreign_tables.read().clone()
    }
    fn domain_view_definitions(&self) -> BTreeMap<RelationIdentity, StoredView> {
        self.durable.views.read().clone()
    }
}
impl DomainTableMetadata for TableState {
    fn domain_columns(&self) -> DomainColumnRead<'_> {
        Box::new(self.columns.read())
    }
    fn domain_table_checks(&self) -> DomainCheckRead<'_> {
        Box::new(self.table_checks.read())
    }
}
impl DomainViewDependencies for Engine {
    fn views_depending_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        Engine::views_depending_on_column(self, table, column)
    }
    fn cascade_view_closure(&self, names: Vec<String>) -> Result<Vec<String>, SQLError> {
        Engine::cascade_view_closure(self, names)
    }
    fn drop_views_inner(&self, names: &[String], cascade: bool) -> Result<(), SQLError> {
        Engine::drop_views_inner(self, names, cascade)
    }
}
impl DomainRegistryPublication for Engine {
    fn persist_domain_definitions(
        &self,
        registry: &BTreeMap<String, StoredDomain>,
    ) -> Result<(), SQLError> {
        self.persist_domains(registry)
    }
    fn publish_domain_definitions(&self, registry: BTreeMap<String, StoredDomain>) {
        *self.durable.domains.write() = registry;
    }
}
impl DomainTableRemoval for Engine {
    fn drop_constraint_dependency(&self, table: &str, name: &str) -> Result<(), SQLError> {
        Engine::drop_constraint_dependency(self, table, name)
    }
    fn clear_column_default(&self, table: &str, column: &str) -> StorageBackendResult<()> {
        self.set_column_default_inner(table, column, None)
            .map(|_| ())
    }
    fn drop_column_cascade(
        &self,
        table: &str,
        column: &str,
        if_exists: bool,
    ) -> Result<(), SQLError> {
        Engine::drop_column_cascade(self, table, column, if_exists)
    }
}
impl DomainForeignRemoval for Engine {
    fn drop_foreign_table_check_dependency(
        &self,
        table: &str,
        name: &str,
    ) -> StorageBackendResult<()> {
        self.foreign_definition_context()
            .drop_foreign_table_check_dependency(table, name)
            .map(|_| ())
    }
    fn clear_foreign_table_default_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        self.foreign_definition_context()
            .clear_foreign_table_default_dependency(table, column)
            .map(|_| ())
    }
    fn drop_foreign_table_column_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        self.foreign_definition_context()
            .drop_foreign_table_column_dependency(table, column)
            .map(|_| ())
    }
}
impl DomainIndexRemoval for Engine {
    fn drop_index_dependency(&self, relation: &RelationIdentity) -> Result<(), SQLError> {
        Engine::drop_index_dependency(self, relation)
    }
}

use uqa_execution::schema::domains::removal::{
    DomainDropNotices, DomainRemovalContext, DomainRoutineRemoval,
};
use uqa_sql::catalog::security::SchemaSecurity;
use uqa_sql::schema::domains::removal::{
    DomainDropAuthority, DomainDropBinding, DomainDropCatalog,
};

impl Engine {
    pub(crate) fn domain_removal_context(&self) -> DomainRemovalContext<'_> {
        DomainRemovalContext {
            refresh: self,
            binding: DomainDropBinding {
                catalog: self,
                authority: self,
                session: self,
            },
            removal: self,
            notices: self,
        }
    }
}
impl DomainDropCatalog for Engine {
    fn schema_security(&self, name: &str) -> Option<SchemaSecurity> {
        self.schema_security_for_privilege(name)
    }
    fn resolve_domain_drop_type(&self, name: &str) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regobject_oid(
            &self.catalog_execution(),
            &ColumnType::Regtype,
            name,
        )
    }
    fn format_domain_drop_type(&self, oid: i64) -> Result<Option<String>, String> {
        uqa_execution::catalog::projection::resolve_regtype_output(
            &self.catalog_execution(),
            &ColumnType::Regtype,
            oid,
        )
    }
}
impl DomainDropAuthority for Engine {
    fn schema_usage(&self, schema: &str, role: &str) -> bool {
        self.schema_has_privilege_for_role(
            schema,
            role,
            uqa_sql::catalog::security::schema::SchemaAclPrivilege::Usage,
        )
    }
    fn current_user_has_role_privileges(&self, role: &str) -> bool {
        Engine::current_user_has_role_privileges(self, role)
    }
}
impl DomainRoutineRemoval for Engine {
    fn remove_domain_types_and_routines(
        &self,
        targets: &BTreeSet<u32>,
        cascade: bool,
    ) -> Result<(), SQLError> {
        self.drop_domain_types_and_routines(targets, cascade)
    }
}
impl DomainDropNotices for Engine {
    fn domain_drop_notice(&self, message: &str) {
        self.push_sql_notice("NOTICE", message);
    }
}
