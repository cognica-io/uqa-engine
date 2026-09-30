//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind relation removal to active registries, session locks and existing publication boundaries.
use crate::Engine;
use uqa_execution::schema::removal::entry::{
    DomainRemovalWrite, DropStatementBindings, SchemaRemovalWrite,
};
use uqa_execution::schema::removal::{
    RelationRemovalContext, RelationRemovalEvents, RelationRemovalLocks, RelationRemovalPrivileges,
    RelationRemovalTransactions, RelationRemovalViews, RelationRemovalWrite,
};
use uqa_sql::{
    catalog::resolution::RelationResolution, schema::removal::RelationDropCatalog, SQLError,
    SQLResult,
};

impl DropStatementBindings for Engine {
    fn with_schema_removal_write(
        &self,
        write: SchemaRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.schema_removal_context()))
    }
    fn with_domain_removal_write(
        &self,
        write: DomainRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.domain_removal_context()))
    }
    fn with_relation_removal_inputs(
        &self,
        run: RelationRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError> {
        run(&self.relation_removal_context())
    }
}

impl Engine {
    pub(crate) fn relation_removal_context(&self) -> RelationRemovalContext<'_> {
        RelationRemovalContext {
            deletion: self,
            catalog: self,
            tables: self.table_removal_context(),
            privileges: self,
            views: self,
            sequences: self,
            identities: self,
            locks: self,
            transactions: self,
            notices: self.query_runtime_view().notices,
            indexes: self.index_removal_context(),
        }
    }
}
impl RelationDropCatalog for Engine {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(name)
    }
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        uqa_execution::catalog::projection::resolve_age_label_relation_name(
            &self.catalog_execution(),
            name,
        )
    }
}
impl RelationRemovalPrivileges for Engine {
    fn ensure_table_drop_authority(&self, table: &str) -> Result<(), SQLError> {
        Engine::ensure_table_drop_authority(self, table)
    }
    fn ensure_foreign_table_drop_authority(&self, table: &str) -> Result<(), SQLError> {
        Engine::ensure_foreign_table_drop_authority(self, table)
    }
}
impl RelationRemovalEvents for Engine {
    fn ensure_no_pending_trigger_events(&self, table: &str, action: &str) -> Result<(), SQLError> {
        Engine::ensure_no_pending_trigger_events(self, table, action)
    }
}
impl RelationRemovalViews for Engine {
    fn lock_dependent_views(&self, names: &[String]) -> Result<(), SQLError> {
        uqa_execution::schema::view_removal::locking::lock_dependent_views(self, self, names)
            .map(|_| ())
    }
    fn ensure_view_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        uqa_execution::schema::view_removal::ensure_view_drop_authorities(
            &self.view_removal_context(),
            &[name.to_string()],
        )
    }
}

impl RelationRemovalLocks for Engine {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError> {
        self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)
    }
}
impl RelationRemovalTransactions for Engine {
    fn with_relation_write(&self, write: RelationRemovalWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_definition_transaction(|engine| {
            write(&engine.relation_removal_context())
        })
    }
}
