//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog metadata adapters for SQL declaration analysis and physical index validation.

use crate::Engine;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::schema::constraints::names::catalog::{
    ConstraintNameCatalog, ConstraintTableMetadata,
};
use uqa_sql::{
    ast::{ColumnDef, Expr, FunctionVolatility},
    SQLError,
};

fn constraint_table_metadata(table: &crate::TableState) -> ConstraintTableMetadata {
    ConstraintTableMetadata {
        object_id: table.object_id(),
        columns: table.columns.snapshot(),
        checks: table.table_checks.snapshot(),
        foreign_keys: table.foreign_keys.snapshot(),
        keys: table.key_constraints.snapshot(),
        hierarchy: table.hierarchy.snapshot(),
    }
}

impl ConstraintNameCatalog for Engine {
    fn constraint_table(&self, relation: &RelationIdentity) -> Option<ConstraintTableMetadata> {
        let table = self.storage.tables.read().get(relation).cloned()?;
        Some(constraint_table_metadata(&table))
    }
    fn constraint_owner(
        &self,
        object_id: [u8; 16],
    ) -> Option<(RelationIdentity, ConstraintTableMetadata)> {
        let (relation, table) = self
            .storage
            .tables
            .read()
            .iter()
            .find(|(_, table)| table.object_id() == object_id)
            .map(|(relation, table)| (relation.clone(), Arc::clone(table)))?;
        Some((relation, constraint_table_metadata(&table)))
    }
    fn constraint_schema_tables(&self, schema: &str) -> Vec<ConstraintTableMetadata> {
        // Release the registry before reading per-table declarations, as catalog capture does.
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(relation, _)| relation.schema == schema)
            .map(|(_, table)| Arc::clone(table))
            .collect::<Vec<_>>();
        tables
            .iter()
            .map(|table| constraint_table_metadata(table))
            .collect()
    }
    fn constraint_foreign_tables(
        &self,
    ) -> Arc<BTreeMap<RelationIdentity, uqa_execution::catalog::foreign::StoredForeignTable>> {
        self.durable.foreign_tables.snapshot()
    }
    fn constraint_domains(&self) -> Arc<uqa_execution::catalog::domain::DomainRegistry> {
        self.durable.domains.snapshot()
    }
    fn constraint_triggers(&self) -> Arc<uqa_sql::catalog::events::TriggerCatalog> {
        self.durable.triggers.snapshot()
    }
}

impl uqa_sql::assignment::AssignmentContext for Engine {
    fn evaluate_domain_check(
        &self,
        expression: &Expr,
        row: &uqa_sql::ResultRow,
        schema: &uqa_sql::RowSchema,
    ) -> Result<uqa_core::Value, SQLError> {
        crate::capabilities::query_expressions::eval_lowered_expression_with_schema(
            self,
            expression,
            row,
            schema,
            &[],
        )
    }
}

impl uqa_sql::schema::SchemaExpressionCatalog for Engine {
    fn plan_schema_expression(
        &self,
        expression: &Expr,
        columns: &[ColumnDef],
    ) -> Result<uqa_sql::schema::expressions::PlannedSchemaExpression, SQLError> {
        uqa_planner::statement_planning::schema_expressions::plan_schema_expression(
            &self.statement_planning_context(),
            self,
            expression,
            columns,
        )
    }

    fn registered_runtime_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        Engine::registered_runtime_function_volatility(self, name)
    }
    fn schema_expression_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        self.describe_table_in_execution(table)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
}

impl uqa_sql::schema::inheritance::InheritanceCatalog for Engine {
    fn resolve_parent(&self, name: &str) -> Result<String, SQLError> {
        uqa_sql::schema::inheritance::inheritance_parent_target(
            self.resolve_visible_relation_kind(name)?,
            name,
        )
    }
    fn declared_constraints(
        &self,
        table: &str,
    ) -> Result<uqa_sql::ast::TableConstraintSet, String> {
        self.try_declared_table_constraints(table)
            .map_err(|error| error.to_string())
    }
    fn check_definitions(&self, table: &str) -> Result<Vec<uqa_sql::ast::TableCheck>, String> {
        self.check_constraint_definitions_in_execution(table)
            .map_err(|error| error.to_string())
    }
    fn unique_index_keys(&self, table: &str) -> Result<Vec<Vec<uqa_sql::ast::IndexKey>>, String> {
        uqa_execution::schema::indexes::registry::lifecycle::standalone_unique_index_keys(
            &self
                .catalog_read_view()
                .snapshot()
                .definitions
                .catalog_indexes,
            table,
        )
        .map_err(|error| error.to_string())
    }
}

impl uqa_sql::schema::indexes::names::IndexNameCatalog for Engine {
    fn automatic_constraint_names(
        &self,
        table: &str,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        self.schema_publication_context()
            .constraint_names()
            .automatic_names(table)
    }
    fn existing_constraint_names(
        &self,
        table: &str,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        self.schema_publication_context()
            .constraint_names()
            .existing_names(table)
    }
    fn existing_constraint_keys(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::ast::TableKeyConstraint>, SQLError> {
        if self.try_resolve_bound_table_name(table)?.is_some() {
            self.key_constraints_in_execution(table)
                .map_err(|error| SQLError::Internal(error.to_string()))
        } else {
            Ok(Vec::new())
        }
    }
    fn relation_name_available(&self, qualified_name: &str) -> Result<bool, SQLError> {
        Ok(matches!(
            self.resolve_bound_relation_kind(qualified_name)?,
            super::RelationResolution::MissingRelation
        ))
    }
}

impl uqa_execution::schema::indexes::IndexBuildCatalog for Engine {
    fn table_hierarchy(&self, table: &str) -> Result<uqa_sql::ast::TableHierarchy, SQLError> {
        self.try_table_hierarchy(table)
            .map_err(|error| uqa_sql::catalog::errors::storage_error("CREATE UNIQUE INDEX", &error))
    }
    fn scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.hierarchy_scan_tables(table, true)
    }
    fn partition_tree(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::semantics::partition::PartitionTreeNode>, SQLError> {
        uqa_sql::semantics::partition::partition_tree(&self.partition_context(), table, false)
    }
}

impl uqa_execution::schema::indexes::unique_build::IndexKeyDescription for Engine {
    fn describe_index_key(
        &self,
        table: &str,
        keys: &[uqa_sql::ast::IndexKey],
        key_types: &[uqa_sql::ColumnType],
        values: &[uqa_core::Value],
    ) -> Result<Option<String>, SQLError> {
        self.constraint_execution_context()
            .describe_index_key(table, keys, key_types, values)
    }
}

impl Engine {
    pub(crate) fn foreign_key_definition_context(
        &self,
    ) -> uqa_sql::schema::foreign_keys::ForeignKeyDefinitionContext<'_> {
        uqa_sql::schema::foreign_keys::ForeignKeyDefinitionContext {
            catalog: self,
            columns: self,
        }
    }
}
impl uqa_sql::schema::foreign_keys::ForeignKeyDefinitionCatalog for Engine {
    fn resolve_table_reference(&self, name: &str) -> Result<String, SQLError> {
        self.resolve_visible_table_reference(name)
    }
    fn bound_table_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.try_resolve_bound_table_name(name)
    }
    fn referenceable_keys(
        &self,
        table: &str,
    ) -> Result<
        Vec<uqa_sql::ast::TableKeyConstraint>,
        uqa_sql::assignment::columns::ColumnCatalogError,
    > {
        Engine::referenceable_keys(self, table).map_err(|error| Box::new(error) as _)
    }
    fn ensure_reference_privilege(&self, table: &str, column: &str) -> Result<(), SQLError> {
        self.ensure_column_privilege(
            table,
            column,
            uqa_sql::catalog::security::table::TableAclPrivilege::References,
        )
    }
}

impl uqa_execution::schema::columns::ColumnRewritePublication for Engine {
    fn update_fields(
        &self,
        table: &str,
        id: uqa_core::DocId,
        values: std::collections::BTreeMap<String, uqa_core::Value>,
        vectors: uqa_execution::mutation::publication::DocumentVectors,
    ) -> Result<bool, SQLError> {
        uqa_execution::schema::columns::generated::update_rewritten_fields(
            &self.generated_rewrite_context(),
            table,
            id,
            values,
            vectors,
        )
    }
}

impl Engine {
    pub(crate) fn schema_dependency_binding_context(
        &self,
    ) -> uqa_sql::schema::dependencies::registration::SchemaDependencyBindingContext<'_> {
        uqa_sql::schema::dependencies::registration::SchemaDependencyBindingContext {
            references: self,
            schema: self,
            bindings: self,
        }
    }
}
impl uqa_sql::schema::dependencies::regclass::SchemaReferenceCatalog for Engine {
    fn loaded_relation_name(&self, reference: &str) -> Result<Option<String>, String> {
        self.resolve_loaded_visible_relation_kind(reference)
            .map(|resolution| resolution.into_found().map(|(canonical, _)| canonical))
            .map_err(|error| error.to_string())
    }
    fn bound_relation_oid(&self, canonical: &str) -> Result<Option<i64>, String> {
        uqa_execution::catalog::projection::resolve_bound_regclass_oid(
            &self.catalog_execution(),
            canonical,
        )
        .map_err(|error| error.to_string())
    }
    fn visible_relation_oid(&self, reference: &str) -> Result<Option<i64>, String> {
        uqa_execution::catalog::projection::resolve_regclass_oid(
            &self.catalog_execution(),
            reference,
        )
        .map_err(|error| error.to_string())
    }
    fn sequence_for_binding(&self, reference: &str) -> Result<String, String> {
        self.resolve_sequence_reference_for_binding(reference)
            .map_err(|error| error.to_string())
    }
}

impl uqa_sql::schema::constraint_views::StoredTableNames for Engine {
    fn stored_table_exists(&self, relation: &uqa_core::RelationIdentity) -> bool {
        self.storage.tables.read().contains_key(relation)
    }
    fn stored_table_names(&self) -> Vec<uqa_core::RelationIdentity> {
        self.storage.tables.read().keys().cloned().collect()
    }
}
