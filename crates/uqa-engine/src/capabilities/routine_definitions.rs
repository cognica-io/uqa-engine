//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine definition analysis to current catalog data and compilation namespaces.

use crate::Engine;
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, CreateFunction},
    binding::{snapshot::BindingSnapshot, stored_relations::StoredQueryNamespace},
    plpgsql::PlpgsqlCatalog,
    routines::{
        compilation::{RoutineCompilationCatalog, RoutineCompilationContext, RoutineParserCatalog},
        declaration::RoutineTypeCatalog,
        dependencies::RoutineCompilationMode,
        merge_columns::StoredMergeColumnCatalog,
        regclass::RoutineRegclassCatalog,
    },
    SQLError,
};

impl Engine {
    pub(crate) fn routine_compilation_context(&self) -> RoutineCompilationContext<'_> {
        RoutineCompilationContext {
            types: self,
            parsers: self,
            catalog: self,
            routines: self,
            relations: self,
            sequences: self,
            merge: self,
            regroles: self,
        }
    }
}
impl RoutineTypeCatalog for Engine {
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table(self, name).map_err(|error| error.to_string())
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            name,
        )
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        uqa_execution::catalog::projection::resolve_catalog_column_type_name(
            &self.catalog_execution(),
            name,
        )
    }
    fn resolve_catalog_domain_type_by_oid(&self, oid: u32) -> Option<ColumnType> {
        uqa_execution::catalog::projection::resolve_catalog_domain_type_by_oid(
            &self.catalog_execution(),
            oid,
        )
    }
}
impl StoredMergeColumnCatalog for Engine {
    fn stored_merge_target_definitions(&self, table: &str) -> Option<Vec<ColumnDef>> {
        self.table_entries()
            .into_iter()
            .find(|(name, _)| name == table)
            .map(|(_, table)| table.columns.read().clone())
    }
}
impl RoutineParserCatalog for Engine {
    fn plpgsql_catalog(&self) -> Result<PlpgsqlCatalog, SQLError> {
        uqa_execution::catalog::projection::plpgsql_catalog(&self.catalog_execution())
    }
}
impl RoutineCompilationCatalog for Engine {
    fn has_registered_aggregate_function(&self, name: &str) -> bool {
        Engine::has_registered_aggregate_function(self, name)
    }
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        uqa_execution::query::binding::binding_context(&scope).map(Into::into)
    }
    fn stored_query_namespace(&self) -> StoredQueryNamespace {
        StoredQueryNamespace {
            temporary_schema: self.temporary_schema_name(),
            transition_relations: crate::sql::active_trigger_transition_relation_names(),
        }
    }
}
impl RoutineRegclassCatalog for Engine {
    fn resolve_routine_regclass(&self, reference: &str) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regclass_oid(
            &self.catalog_execution(),
            reference,
        )
    }
}

use uqa_core::RelationIdentity;
use uqa_execution::routines::{
    compilation::{self, RoutineCompilationSession, StoredRoutineCompilationContext},
    definition::{self, RoutineDefinitionContext},
    rewrites::{self, RoutineRewriteContext},
};
use uqa_sql::{ast::FunctionBinding, routines::CompiledFunctionBody};

impl Engine {
    pub(crate) fn routine_definition_context(&self) -> RoutineDefinitionContext<'_> {
        RoutineDefinitionContext {
            compilation: self.stored_routine_compilation_context(),
            sources: self,
            regclasses: self,
        }
    }
    pub(crate) fn compile_catalog_bound_routine(
        &self,
        def: &mut CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<(CompiledFunctionBody, bool), SQLError> {
        definition::compile_catalog_bound_routine(&self.routine_definition_context(), def, mode)
    }
    pub(crate) fn stored_routine_compilation_context(&self) -> StoredRoutineCompilationContext<'_> {
        StoredRoutineCompilationContext {
            analysis: self.routine_compilation_context(),
            session: self,
        }
    }
    pub(crate) fn routine_rewrite_context(&self) -> RoutineRewriteContext<'_> {
        RoutineRewriteContext {
            registry: self,
            publication: self,
            compilation: self.stored_routine_compilation_context(),
            columns: self.stored_column_binding_context(),
            changes: self,
        }
    }
    pub(crate) fn compile_persisted_sql_function(
        &self,
        def: &CreateFunction,
    ) -> Result<CompiledFunctionBody, SQLError> {
        compilation::compile_persisted_sql_function(&self.stored_routine_compilation_context(), def)
    }
    pub(crate) fn rewrite_routine_relation_references(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<(), SQLError> {
        rewrites::rewrite_routine_relation_references(&self.routine_rewrite_context(), from, to)
    }
    pub(crate) fn rewrite_routine_column_references(
        &self,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        rewrites::rewrite_routine_column_references(
            &self.routine_rewrite_context(),
            relation,
            from,
            to,
        )
    }
    pub(crate) fn publish_stored_routine_body_rewrites(
        &self,
        definitions: Vec<CreateFunction>,
    ) -> Result<(), SQLError> {
        rewrites::publish_stored_routine_body_rewrites(&self.routine_rewrite_context(), definitions)
    }
    pub(crate) fn refresh_stored_merge_target_plans(&self) -> Result<(), SQLError> {
        rewrites::refresh_stored_merge_target_plans(&self.routine_rewrite_context())
    }
    pub(crate) fn prepare_routine_column_alias_drop(
        &self,
        columns: BTreeSet<(String, String)>,
        removed: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError> {
        uqa_execution::routines::removal::prepare_routine_column_alias_drop(
            &self.routine_removal_context(),
            columns,
            removed,
        )
    }
}
impl RoutineCompilationSession for Engine {
    fn routine_search_path(&self) -> Vec<String> {
        self.session.state.read().search_path.clone()
    }
    fn replace_routine_search_path(&self, path: Vec<String>) -> Vec<String> {
        std::mem::replace(&mut self.session.state.write().search_path, path)
    }
    fn restore_routine_search_path(&self, path: Vec<String>) {
        self.session.state.write().search_path = path;
    }
}

use uqa_execution::routines::{
    catalog::RoutineMutationContext,
    configuration::{RoutineConfigurationGuard, RoutineConfigurationSession},
    registration::{self, RoutineCreationNamespace, RoutineRegistrationContext},
};
use uqa_sql::{ast::AlterRoutineStmt, routines::registration::RoutineSupportAuthority};

impl RoutineCreationNamespace for Engine {
    fn routine_name_for_create(&self, name: &str) -> Result<String, SQLError> {
        self.try_relation_name_for_sql_create(name)
    }
}
impl RoutineSupportAuthority for Engine {
    fn current_user_is_superuser(&self) -> bool {
        Engine::current_user_is_superuser(self)
    }
}
impl RoutineConfigurationGuard for crate::roles::RoutineSessionStateGuard<'_> {}
impl RoutineConfigurationSession for Engine {
    fn routine_configuration_guard(&self) -> Box<dyn RoutineConfigurationGuard + '_> {
        Box::new(self.routine_config_state_guard())
    }
    fn set_routine_variable(&self, name: &str, value: &str) -> Result<(), SQLError> {
        self.set_variable(name, value)
    }
    fn show_routine_variable(&self, name: &str) -> Result<String, SQLError> {
        self.show_variable(name)
    }
}
impl Engine {
    pub(crate) fn routine_mutation_context(&self) -> RoutineMutationContext<'_> {
        RoutineMutationContext {
            writer: self,
            names: self,
            roles: self,
            registry: self,
            publication: self,
            changes: self,
        }
    }
    fn routine_registration_context(&self) -> RoutineRegistrationContext<'_> {
        RoutineRegistrationContext {
            catalog: self.routine_mutation_context(),
            namespace: self,
            definition: self.routine_definition_context(),
            support: self,
            configuration: self,
        }
    }
    pub(crate) fn register_sql_function(&self, def: CreateFunction) -> Result<(), SQLError> {
        registration::register_sql_function(&self.routine_registration_context(), def)
    }
    pub(crate) fn alter_sql_routine(&self, stmt: &AlterRoutineStmt) -> Result<(), SQLError> {
        registration::alter_sql_routine(&self.routine_registration_context(), stmt)
    }
}
