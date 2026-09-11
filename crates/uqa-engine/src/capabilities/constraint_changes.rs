//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind constraint execution to transaction modes, relation permissions, and catalog state.
use crate::Engine;
use uqa_execution::schema::constraints::{
    ConstraintAlterAccess, ConstraintAlterContext, ConstraintModes, ConstraintRelations,
};
use uqa_sql::{
    ast::{ForeignKey, TableHierarchy},
    catalog::constraints::ConstraintIdentity,
    SQLError,
};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn constraint_alter_context(&self) -> ConstraintAlterContext<'_> {
        let runtime = self.query_runtime_view();
        ConstraintAlterContext {
            catalog: self,
            relations: self,
            access: self,
            locks: self,
            modes: self,
            names: self,
            rows: self.constraint_execution_context(),
            foreign_keys: self.foreign_key_definition_context(),
            publication: self.schema_publication_context(),
            writes: self,
            notices: runtime.notices,
        }
    }
    pub(crate) fn constraint_type_context(
        &self,
    ) -> uqa_sql::schema::constraint_changes::ConstraintTypeContext<'_> {
        uqa_sql::schema::constraint_changes::ConstraintTypeContext {
            foreign_keys: self.foreign_key_definition_context(),
            referrers: self,
        }
    }
}
impl uqa_sql::schema::constraint_changes::ConstraintTypeReferrers for Engine {
    fn try_referrers_to(
        &self,
        table: &str,
    ) -> Result<Vec<(String, ForeignKey)>, uqa_sql::assignment::columns::ColumnCatalogError> {
        Engine::try_referrers_to(self, table).map_err(|error| Box::new(error) as _)
    }
}
impl ConstraintRelations for Engine {
    fn table_names(&self) -> StorageBackendResult<Vec<String>> {
        Engine::table_names(self)
    }
    fn table_hierarchy(&self, table: &str) -> StorageBackendResult<TableHierarchy> {
        self.try_table_hierarchy(table)
    }
}
impl ConstraintModes for Engine {
    fn prune(&self) -> Result<(), SQLError> {
        self.prune_constraint_modes()
    }
    fn forget(&self, identity: &ConstraintIdentity) {
        self.forget_named_constraint_mode(identity);
    }
}
impl ConstraintAlterAccess for Engine {
    fn ensure_table_owner(&self, table: &str) -> Result<(), SQLError> {
        Engine::ensure_table_owner(self, table).map(|_| ())
    }
    fn ensure_no_pending_events(&self, table: &str, action: &str) -> Result<(), SQLError> {
        self.ensure_no_pending_trigger_events(table, action)
    }
    fn constraint_trigger_name(&self, table: &str, name: &str) -> Result<Option<String>, SQLError> {
        self.event_lookup_context()
            .constraint_trigger_by_constraint_name(table, name)
            .map(|trigger| trigger.map(|trigger| trigger.definition.name))
    }
}
