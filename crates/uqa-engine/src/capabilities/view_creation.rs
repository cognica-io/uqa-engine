//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind view lifecycle operations to actual transaction, registry, and role state.
use crate::Engine;
use uqa_execution::schema::view_creation::{
    self,
    context::{
        MaterializedViewAccess, MaterializedViewWrite, ViewCreationCatalog, ViewCreationContext,
        ViewCreationTransactions, ViewCreationWrite, ViewOwnerQuery, ViewPlanBinding,
        ViewQueryOwners,
    },
};
use uqa_sql::{
    catalog::stored_view::StoredView, plan::QueryPlan, RowSchema, SQLError, SQLParam, SQLResult,
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub fn register_view(
        &self,
        name: &str,
        body: uqa_sql::ast::SelectStmt,
    ) -> Result<(), SQLError> {
        view_creation::register_view(self, name, body)
    }
    fn view_creation_context(&self) -> ViewCreationContext<'_> {
        ViewCreationContext {
            catalog: self,
            views: self,
            namespace: self.relation_creation_context(),
            names: self,
            owners: self,
            access: self,
            bindings: self,
            routines: self,
            regroles: self,
            rewrite: self.view_rewrite_context(),
            queries: self,
            query_owners: self,
            publication: self,
            changes: self,
        }
    }
}
impl ViewCreationTransactions for Engine {
    fn with_view_creation(&self, write: ViewCreationWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.view_creation_context()))
    }
    fn with_materialized_view_creation(
        &self,
        write: MaterializedViewWrite<'_>,
    ) -> Result<Option<u64>, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.view_creation_context()))
    }
}
impl ViewCreationCatalog for Engine {
    fn synchronize(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
}
impl ViewPlanBinding for Engine {
    fn bind_relations(&self, plan: &mut QueryPlan) -> Result<bool, SQLError> {
        self.bind_stored_query_relations(plan, "CREATE VIEW", true)
    }
    fn bind_routines(
        &self,
        plan: &mut QueryPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        uqa_execution::query::binding::bind_query_plan_routines_for_storage(
            self, plan, params, &scope, None,
        )
    }
}

impl MaterializedViewAccess for Engine {
    fn current_user_name(&self) -> String {
        self.current_user_name()
    }

    fn ensure_maintenance(&self, name: &str, view: &StoredView) -> Result<(), SQLError> {
        uqa_sql::catalog::security::view_ownership::ensure_materialized_view_maintenance(
            self.view_ownership_context(),
            name,
            view,
        )
    }
}
impl ViewQueryOwners for Engine {
    fn with_owner(
        &self,
        owner: &str,
        operation: ViewOwnerQuery<'_>,
    ) -> Result<SQLResult, SQLError> {
        self.with_current_user_context(owner, || operation(self))
    }
}

impl Engine {
    pub(crate) fn view_ownership_context(
        &self,
    ) -> uqa_sql::catalog::security::view_ownership::ViewOwnershipContext<'_> {
        uqa_sql::catalog::security::view_ownership::ViewOwnershipContext {
            session: self,
            roles: self,
            schemas: self,
        }
    }
    pub(crate) fn ensure_view_owner(
        &self,
        name: &str,
        view: &StoredView,
    ) -> Result<String, SQLError> {
        uqa_sql::catalog::security::view_ownership::ensure_view_owner(
            self.view_ownership_context(),
            name,
            view,
        )
    }
}
impl uqa_sql::catalog::security::view_ownership::ViewOwnerSchemas for Engine {
    fn schema_security(&self, schema: &str) -> Option<uqa_sql::catalog::security::SchemaSecurity> {
        self.schema_security_for_privilege(schema)
    }
}

impl uqa_execution::catalog::view::ViewIdentityAllocation for Engine {
    fn allocate_identity(&self) -> StorageBackendResult<[u8; 16]> {
        crate::new_view_object_id()
    }
}
