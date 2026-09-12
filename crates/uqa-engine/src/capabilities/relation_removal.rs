//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind relation removal to active registries, session locks and existing publication boundaries.
use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_execution::schema::removal::entry::{
    DomainRemovalWrite, DropStatementBindings, SchemaRemovalWrite,
};
use uqa_execution::schema::removal::{
    RelationRemovalContext, RelationRemovalEvents, RelationRemovalLocks, RelationRemovalPrivileges,
    RelationRemovalRoutines, RelationRemovalTransactions, RelationRemovalViews,
    RelationRemovalWrite,
};
use uqa_sql::{
    catalog::{errors::storage_error, resolution::RelationResolution},
    schema::removal::{ForeignTableDropDependencies, RelationDropCatalog},
    SQLError, SQLResult,
};
use uqa_storage::StorageBackendResult;

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
            catalog: self,
            dependencies: self,
            tables: self.table_removal_context(),
            privileges: self,
            routines: self,
            events: self,
            foreign_tables: self.foreign_removal_context(),
            views: self,
            sequences: self,
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
impl ForeignTableDropDependencies for Engine {
    fn views_depending_on_relation(&self, name: &str) -> Result<Vec<String>, SQLError> {
        Engine::views_depending_on_relation(self, name)
            .map_err(|error| storage_error("DROP FOREIGN TABLE dependency preflight", &error))
    }
    fn rules_depending_on_relations(
        &self,
        names: &[String],
    ) -> Result<Vec<(RelationIdentity, String)>, SQLError> {
        self.event_lookup_context()
            .rules_depending_on_relations(names)
            .map_err(uqa_storage::StorageBackendError::Other)
            .map_err(|error| storage_error("DROP FOREIGN TABLE dependency preflight", &error))
    }
    fn sequence_external_dependents_for_owner_drop(
        &self,
        name: &str,
        targets: &BTreeSet<String>,
    ) -> Result<Vec<String>, SQLError> {
        Engine::sequence_external_dependents_for_owner_drop(self, name, targets).map_err(|error| {
            storage_error(
                "DROP FOREIGN TABLE owned-sequence dependency preflight",
                &error,
            )
        })
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
impl RelationRemovalRoutines for Engine {
    fn drop_relation_routine_dependents(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError> {
        Engine::drop_relation_routine_dependents(self, names, cascade, kind)
    }
}
impl RelationRemovalEvents for Engine {
    fn ensure_no_pending_trigger_events(&self, table: &str, action: &str) -> Result<(), SQLError> {
        Engine::ensure_no_pending_trigger_events(self, table, action)
    }
    fn drop_rules_depending_on_relations_inner(
        &self,
        names: &[String],
    ) -> StorageBackendResult<()> {
        self.event_lifecycle_context()
            .drop_rules_depending_on_relations_inner(names)
    }
}
impl RelationRemovalViews for Engine {
    fn drop_views(&self, names: &[String], cascade: bool, kind: &str) -> Result<(), SQLError> {
        Engine::drop_views(self, names, cascade, kind)
    }
    fn drop_views_depending_on_relations(&self, names: &[String]) -> StorageBackendResult<()> {
        Engine::drop_views_depending_on_relations(self, names)
    }
}

impl RelationRemovalLocks for Engine {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError> {
        self.lock_relation(table, crate::row_locks::RelationLockMode::AccessExclusive)
    }
}
impl RelationRemovalTransactions for Engine {
    fn with_relation_write(&self, write: RelationRemovalWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.relation_removal_context()))
    }
}
