//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog projection inputs assembled at a statement boundary.

mod query_reads;

use super::cache::RegtypeOutputCache;
use super::security::{
    schema::{role_has_schema_privilege, SchemaAclPrivilege},
    BoundSchemaSecurity,
};
use super::services::{
    CatalogExpressionEvaluation, CatalogNamespace, CatalogSession, CatalogSnapshotSource,
    RelationCounts, ViewCatalogCapabilities,
};
use super::{CatalogReadView, RelationLookupMode, RelationNameResolution, RelationResolution};
use uqa_sql::catalog::roles::identity::RoleSubject;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::routines::RoutineResolution;
use uqa_sql::{ColumnType, SQLError};

#[derive(Clone, Copy)]
pub struct CatalogContext<'a> {
    pub catalog: &'a dyn CatalogSnapshotSource,
    pub session: &'a dyn CatalogSession,
    pub namespaces: &'a dyn CatalogNamespace,
    pub routines: &'a dyn RoutineResolution,
    pub expressions: &'a dyn CatalogExpressionEvaluation,
    pub counts: &'a dyn RelationCounts,
    pub views: &'a dyn ViewCatalogCapabilities,
    pub cache: &'a RegtypeOutputCache,
}

impl CatalogContext<'_> {
    pub fn catalog_read_view(&self) -> CatalogReadView {
        self.catalog.catalog_snapshot()
    }
    pub fn session_execution_view(&self) -> &dyn CatalogSession {
        self.session
    }
    pub fn current_schema_names(&self, implicit: bool) -> Result<Vec<String>, SQLError> {
        self.namespaces.current_schema_names(implicit)
    }
    pub fn current_role(&self) -> RoleReference {
        self.session.current_role()
    }
    pub fn search_path_contains(&self, schema: &str) -> bool {
        self.session
            .relation_name_resolution()
            .search_path()
            .iter()
            .any(|name| name == schema)
    }
    pub fn stored_view_schema_with_catalog(
        &self,
        view: &super::view::StoredView,
        catalog: CatalogReadView,
        resolution: RelationNameResolution,
    ) -> Result<crate::RowSchema, SQLError> {
        view.row_schema(self.routines, std::sync::Arc::new(catalog), resolution)
    }
    pub fn table_doc_count(&self, name: &str) -> Result<u64, SQLError> {
        self.counts.table_doc_count(name)
    }
    pub fn resolve_bound_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        let mut resolution = self.session.relation_name_resolution();
        resolution.set_lookup_mode(RelationLookupMode::Bound);
        self.catalog_read_view()
            .relation_kind_resolution(&resolution, name)
    }
    pub fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.catalog
            .refreshed_catalog_snapshot()?
            .relation_kind_resolution(&self.session.relation_name_resolution(), name)
            .map(RelationResolution::into_found)
    }
    pub fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, SQLError> {
        Ok(self
            .resolve_bound_relation_kind(name)?
            .into_found()
            .map(|(_, kind)| kind))
    }
    pub fn sequence_object_id(&self, name: &str) -> Result<Option<[u8; 16]>, SQLError> {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        Ok(self
            .catalog_read_view()
            .snapshot()
            .definitions
            .sequence_object_ids
            .get(&relation)
            .copied())
    }
    pub fn resolve_domain_type(&self, name: &str) -> Option<ColumnType> {
        let names = uqa_sql::compiler::parse_regobject_name(name)?;
        let catalog = self.catalog_read_view();
        let domains = &catalog.snapshot().definitions.domains;
        if let [schema, local] = names.as_slice() {
            return domains
                .values()
                .find(|domain| domain.identity.schema == *schema && domain.identity.name == *local)
                .map(uqa_sql::catalog::domain::StoredDomain::column_type);
        }
        let [local] = names.as_slice() else {
            return None;
        };
        for schema in self.session.relation_name_resolution().search_path() {
            if let Some(domain) = domains
                .values()
                .find(|domain| domain.identity.schema == *schema && domain.identity.name == *local)
            {
                return Some(domain.column_type());
            }
        }
        None
    }
    pub fn schema_security_for_privilege(&self, schema: &str) -> Option<BoundSchemaSecurity> {
        self.schema_security_in(&self.catalog_read_view(), schema)
    }
    fn schema_security_in(
        &self,
        catalog: &CatalogReadView,
        schema: &str,
    ) -> Option<BoundSchemaSecurity> {
        if let Some(security) = catalog.schema_security(schema) {
            return Some(security.clone());
        }
        match schema {
            "pg_catalog" | "information_schema" => {
                Some(BoundSchemaSecurity::with_public_privileges(false))
            }
            "ag_catalog" => Some(BoundSchemaSecurity::bootstrap("ag_catalog")),
            name if name == self.session.temporary_schema_name() => {
                Some(BoundSchemaSecurity::with_public_privileges(true))
            }
            name if catalog.snapshot().definitions.graphs.contains_key(name) => {
                Some(BoundSchemaSecurity::bootstrap(name))
            }
            _ => None,
        }
    }
    pub fn require_schema_privilege(
        &self,
        schema: &str,
        role: &(impl RoleSubject + ?Sized),
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        let catalog = self.catalog_read_view();
        let definitions = &catalog.snapshot().definitions;
        if self
            .schema_security_in(&catalog, schema)
            .map(|security| {
                security
                    .resolve(&definitions.roles)
                    .map_err(SQLError::Internal)
            })
            .transpose()?
            .is_some_and(|security| {
                role_has_schema_privilege(
                    &security,
                    role,
                    privilege,
                    &definitions.roles,
                    &definitions.role_memberships,
                )
            })
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for schema {schema}"),
        })
    }
}
