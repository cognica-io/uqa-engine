//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable schemas for predicate-placement tests.

use super::super::context::{FilterPushdownContext, FilterPushdownScope};
use std::sync::Arc;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility},
    catalog::{
        analysis::{AnalysisCatalog, TableDefinition, ViewDefinition},
        resolution::{RelationLookupMode, RelationNameResolution},
    },
    plan::QueryPlan,
    routines::SQLUserFunction,
    semantics::volatility::VolatilityCatalog,
    ColumnType, SQLError,
};

pub(super) struct Fixture {
    resolution: RelationNameResolution,
    table: TableDefinition,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let uqa_sql::ast::Statement::CreateTable(definition) =
            uqa_sql::compile("CREATE TABLE filter_alias_source(id INTEGER, label TEXT)")
                .unwrap()
                .remove(0)
        else {
            panic!("expected table definition")
        };
        Self {
            resolution: RelationNameResolution {
                search_path: vec!["public".into()],
                temporary_schema: "pg_temp_1".into(),
                temporary_namespace_allocated: false,
                current_user: "postgres".into(),
                lookup_mode: RelationLookupMode::Dynamic,
            },
            table: TableDefinition {
                columns: Arc::new(definition.columns),
                columns_declared: true,
            },
        }
    }
    pub(super) fn context(&self) -> FilterPushdownContext<'_> {
        FilterPushdownContext {
            volatility: self,
            correlation: uqa_sql::binding::correlation::CorrelationContext {
                catalog: self,
                resolution: &self.resolution,
            },
            optimizer: &Ok,
        }
    }
    pub(super) fn scope(&self) -> FilterPushdownScope<'_> {
        FilterPushdownScope {
            catalog: self,
            resolution: &self.resolution,
            is_visible_cte: &|_| false,
        }
    }
}
impl AnalysisCatalog for Fixture {
    fn table_resolved(
        &self,
        _resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok((name == "filter_alias_source").then(|| self.table.clone()))
    }
    fn table_name_resolved(
        &self,
        _resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok((name == "filter_alias_source").then(|| name.to_string()))
    }
    fn view_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        Ok(None)
    }
    fn foreign_table_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok(None)
    }
    fn sequence_exists(&self, _: &RelationNameResolution, _: &str) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        Ok(None)
    }
    fn sql_functions(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(None)
    }
}
impl VolatilityCatalog for Fixture {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}
