//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog metadata adapters for SQL declaration analysis and physical index validation.

use crate::Engine;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, Expr, FunctionVolatility},
    SQLError,
};

impl uqa_sql::schema::SchemaExpressionCatalog for Engine {
    fn registered_runtime_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        Engine::registered_runtime_function_volatility(self, name)
    }
    fn schema_expression_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        self.try_describe_table(table)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
}

impl uqa_sql::schema::inheritance::InheritanceCatalog for Engine {
    fn resolve_parent(&self, name: &str) -> Result<String, SQLError> {
        self.resolve_visible_table_reference(name)
    }
    fn declared_constraints(
        &self,
        table: &str,
    ) -> Result<uqa_sql::ast::TableConstraintSet, String> {
        self.try_declared_table_constraints(table)
            .map_err(|error| error.to_string())
    }
    fn check_definitions(&self, table: &str) -> Result<Vec<uqa_sql::ast::TableCheck>, String> {
        self.try_check_constraint_definitions(table)
            .map_err(|error| error.to_string())
    }
}

impl uqa_sql::schema::indexes::names::IndexNameCatalog for Engine {
    fn existing_constraint_keys(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::ast::TableKeyConstraint>, SQLError> {
        if self.try_resolve_bound_table_name(table)?.is_some() {
            self.try_key_constraints(table)
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
            crate::table_security::TableAclPrivilege::References,
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
        self.update_document_fields_with_vector_values(table, id, values, vectors)
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

impl Engine {
    pub(crate) fn validate_default_expression(
        &self,
        expression: &mut Expr,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let scope = crate::capabilities::query_scope::new_for_catalog_binding(self);
        let binding = uqa_execution::query::binding::binding_context(&scope)?;
        uqa_sql::schema::defaults::validate_default_expression(
            &uqa_sql::schema::SchemaBindingContext {
                catalog: self,
                binding: &binding,
            },
            expression,
            target,
        )
    }

    pub(crate) fn validate_check_expression(
        &self,
        table: &str,
        qualifier: &str,
        columns: &[ColumnDef],
        expression: &mut Expr,
    ) -> Result<(), SQLError> {
        let scope = crate::capabilities::query_scope::new_for_catalog_binding(self);
        let binding = uqa_execution::query::binding::binding_context(&scope)?;
        uqa_sql::schema::constraints::validate_check_expression(
            &uqa_sql::schema::SchemaBindingContext {
                catalog: self,
                binding: &binding,
            },
            table,
            qualifier,
            columns,
            expression,
        )
    }
}
