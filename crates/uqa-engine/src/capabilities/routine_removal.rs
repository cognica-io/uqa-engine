//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine namespace lookup to live session and schema authorization state.

use crate::{schema_security::SchemaAclPrivilege, Engine};
use uqa_sql::{
    catalog::security::SchemaSecurity, routines::lifecycle::names::RoutineNameCatalog, SQLError,
};

impl RoutineNameCatalog for Engine {
    fn schema_security(&self, schema: &str) -> Option<SchemaSecurity> {
        self.schema_security_for_privilege(schema)
    }
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn search_path(&self) -> Vec<String> {
        self.session.state.read().search_path.clone()
    }
    fn require_schema_usage(&self, schema: &str, role: &str) -> Result<(), SQLError> {
        self.require_schema_privilege(schema, role, SchemaAclPrivilege::Usage)
    }
    fn schema_has_usage(&self, schema: &str, role: &str) -> bool {
        self.schema_has_privilege_for_role(schema, role, SchemaAclPrivilege::Usage)
    }
}

use std::{collections::BTreeSet, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::routines::removal::{
    self,
    context::{
        RoutineCheckRead, RoutineColumnRead, RoutineDependencyCatalog, RoutineDependencyContext,
        RoutineDropNotices, RoutineEventDependencies, RoutineEventRemoval, RoutineForeignRead,
        RoutineForeignRemoval, RoutineIndexDependencies, RoutineRegistryPublication,
        RoutineRegistryState, RoutineRegistryWrite, RoutineRemovalContext,
        RoutineSequenceDependencies, RoutineTableMetadata, RoutineTableRemoval,
        RoutineViewDependencies,
    },
};
use uqa_sql::{
    ast::{DropRule, DropTrigger, FunctionBinding},
    routines::lifecycle::RoutineRegistry,
    schema::sequences::dependents::SequenceSchemaDependent,
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn routine_removal_context(&self) -> RoutineRemovalContext<'_> {
        RoutineRemovalContext {
            names: self,
            registry: self,
            publication: self,
            roles: self,
            catalog: self.catalog_execution(),
            domains: self.domain_dependency_context(),
            dependencies: RoutineDependencyContext {
                catalog: self,
                views: self,
                events: self,
                indexes: self,
                sequences: self,
                columns: self.stored_column_binding_context(),
            },
            bodies: self.routine_rewrite_context(),
            tables: self,
            foreign: self,
            events: self,
            notices: self,
            changes: self,
        }
    }
    #[cfg(test)]
    pub(crate) fn drop_sql_functions(
        &self,
        statement: &uqa_sql::ast::DropFunctionStmt,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            removal::drop_sql_functions(&engine.routine_removal_context(), statement)
        })
    }
    #[cfg(test)]
    pub(crate) fn preflight_sql_function_drop(
        &self,
        statement: &uqa_sql::ast::DropFunctionStmt,
    ) -> Result<uqa_sql::routines::lifecycle::SQLFunctionDropPlan, SQLError> {
        removal::preflight_sql_function_drop(&self.routine_removal_context(), statement)
    }
    #[cfg(test)]
    pub(crate) fn commit_sql_function_drop(
        &self,
        plan: uqa_sql::routines::lifecycle::SQLFunctionDropPlan,
    ) -> Result<(), SQLError> {
        removal::commit_sql_function_drop(&self.routine_removal_context(), plan)
    }
    pub(crate) fn drop_domain_types_and_routines(
        &self,
        targets: &BTreeSet<u32>,
        cascade: bool,
    ) -> Result<(), SQLError> {
        removal::drop_domain_types_and_routines(&self.routine_removal_context(), targets, cascade)
    }
    pub(crate) fn drop_schema_types_and_routines(
        &self,
        schemas: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        removal::drop_schema_types_and_routines(&self.routine_removal_context(), schemas)
    }
    pub(crate) fn drop_column_routine_dependents(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        removal::drop_column_routine_dependents(
            &self.routine_removal_context(),
            table,
            column,
            cascade,
        )
    }
    pub(crate) fn drop_relation_routine_dependents(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError> {
        removal::drop_relation_routine_dependents(
            &self.routine_removal_context(),
            names,
            cascade,
            kind,
        )
    }
}
impl RoutineRegistryState for Engine {
    fn routine_snapshot(&self) -> RoutineRegistry {
        self.durable.sql_user_functions.read().clone()
    }
    fn routines_write(&self) -> RoutineRegistryWrite<'_> {
        Box::new(self.durable.sql_user_functions.write())
    }
}
impl RoutineRegistryPublication for Engine {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError> {
        self.persist_sql_functions_snapshot(registry)
    }
}
impl RoutineDependencyCatalog for Engine {
    fn routine_table_schemas(&self) -> Vec<(String, Arc<dyn RoutineTableMetadata>)> {
        self.table_entries()
            .into_iter()
            .map(|(name, table)| (name, table as Arc<dyn RoutineTableMetadata>))
            .collect()
    }
    fn routine_foreign_tables(&self) -> RoutineForeignRead<'_> {
        Box::new(self.durable.foreign_tables.read())
    }
}
impl RoutineTableMetadata for crate::TableState {
    fn object_id(&self) -> [u8; 16] {
        crate::TableState::object_id(self)
    }
    fn columns(&self) -> RoutineColumnRead<'_> {
        Box::new(self.columns.read())
    }
    fn table_checks(&self) -> RoutineCheckRead<'_> {
        Box::new(self.table_checks.read())
    }
}
impl RoutineViewDependencies for Engine {
    fn views_depending_on_function(
        &self,
        target: &FunctionBinding,
    ) -> StorageBackendResult<Vec<String>> {
        Engine::views_depending_on_function(self, target)
    }
    fn views_depending_on_relation(&self, relation: &str) -> StorageBackendResult<Vec<String>> {
        Engine::views_depending_on_relation(self, relation)
    }
    fn views_depending_on_sequence(&self, sequence: &str) -> StorageBackendResult<Vec<String>> {
        Engine::views_depending_on_sequence(self, sequence)
    }
}
impl RoutineEventDependencies for Engine {
    fn triggers_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<(String, String)>, SQLError> {
        self.event_lookup_context()
            .triggers_depending_on_routine(target)
    }
    fn rules_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        self.event_lookup_context()
            .rules_depending_on_routine(target)
            .map_err(uqa_storage::StorageBackendError::Other)
    }
    fn rules_depending_on_relations(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        self.event_lookup_context()
            .rules_depending_on_relations(relations)
            .map_err(uqa_storage::StorageBackendError::Other)
    }
}
impl RoutineIndexDependencies for Engine {
    fn indexes_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<RelationIdentity>, SQLError> {
        Engine::indexes_depending_on_routine(self, target)
    }
}
impl RoutineSequenceDependencies for Engine {
    fn sequence_names(&self) -> Vec<String> {
        self.durable
            .sequences
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect()
    }
    fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>> {
        Engine::sequence_schema_expression_dependents(self, sequence)
    }
    fn sequence_names_owned_by_tables(
        &self,
        owners: &BTreeSet<[u8; 16]>,
    ) -> StorageBackendResult<BTreeSet<String>> {
        Engine::sequence_names_owned_by_tables(self, owners)
    }
    fn sequence_names_owned_by_column(
        &self,
        table: [u8; 16],
        column: [u8; 16],
    ) -> StorageBackendResult<BTreeSet<String>> {
        Engine::sequence_names_owned_by_column(self, table, column)
    }
}
impl RoutineTableRemoval for Engine {
    fn set_column_default_none(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.set_column_default_inner(table, column, None)
    }
    fn try_drop_column_inner(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        Engine::try_drop_column_inner(self, table, column)
    }
}
impl RoutineForeignRemoval for Engine {
    fn drop_foreign_table_check_dependency(
        &self,
        table: &str,
        constraint: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.foreign_definition_context()
            .drop_foreign_table_check_dependency(table, constraint)
    }
    fn clear_foreign_table_default_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.foreign_definition_context()
            .clear_foreign_table_default_dependency(table, column)
    }
    fn drop_foreign_table_generated_column_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.foreign_definition_context()
            .drop_foreign_table_column_dependency(table, column)
    }
}
impl RoutineEventRemoval for Engine {
    fn drop_rule(&self, statement: &DropRule) -> Result<(), SQLError> {
        self.event_lifecycle_context().drop_rule(statement)
    }
    fn drop_trigger(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        self.event_lifecycle_context().drop_trigger(statement)
    }
}
impl RoutineDropNotices for Engine {
    fn routine_drop_notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
