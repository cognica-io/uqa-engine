//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned parsed, logical, and optimized SQL statement cache.

use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
use std::sync::Arc;

use super::engine_capabilities::CatalogEpochs;
use super::Engine;

const SQL_STATEMENT_CACHE_LIMIT: usize = 256;

#[derive(Clone, Default)]
pub(super) struct SQLStatementCache {
    entries: BTreeMap<String, CachedSQLStatement>,
    insertion_order: VecDeque<String>,
}

#[derive(Clone)]
pub(crate) struct CachedSQLStatement {
    pub(crate) statement: Arc<uqa_sql::ast::Statement>,
    pub(crate) logical_plan: Arc<uqa_planner::UnifiedPlan>,
    pub(crate) optimized_plan: Option<Arc<uqa_planner::UnifiedPlan>>,
    catalog_epochs: CatalogEpochs,
}

#[derive(Clone)]
pub(super) struct PreparedStatementPlan {
    pub(super) logical_plan: Arc<uqa_planner::UnifiedPlan>,
    pub(super) plan: Option<uqa_planner::UnifiedPlan>,
    pub(super) parameter_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    pub(super) result_schema: Option<uqa_execution::RowSchema>,
    pub(crate) source_sql: Option<Arc<str>>,
    pub(crate) prepared_at_micros: i64,
    pub(crate) from_sql: bool,
    pub(crate) generic_plans: i64,
    pub(crate) custom_plans: i64,
    pub(super) generic_cost: Option<f64>,
    pub(super) total_custom_cost: f64,
}

/// Session catalog data without executable plans or planner ownership.
pub(crate) struct PreparedStatementMetadata {
    pub(crate) name: String,
    pub(crate) parameter_types: Vec<Option<uqa_sql::ColumnType>>,
    pub(crate) result_types: Option<Vec<Option<uqa_sql::ColumnType>>>,
    pub(crate) source_sql: Option<Arc<str>>,
    pub(crate) prepared_at_micros: i64,
    pub(crate) from_sql: bool,
    pub(crate) generic_plans: i64,
    pub(crate) custom_plans: i64,
}

impl PreparedStatementPlan {
    pub(super) fn metadata(&self, name: &str) -> PreparedStatementMetadata {
        PreparedStatementMetadata {
            name: name.to_string(),
            parameter_types: self.parameter_types.clone(),
            result_types: self
                .result_schema
                .as_ref()
                .map(|schema| schema.column_types().to_vec()),
            source_sql: self.source_sql.clone(),
            prepared_at_micros: self.prepared_at_micros,
            from_sql: self.from_sql,
            generic_plans: self.generic_plans,
            custom_plans: self.custom_plans,
        }
    }
}

impl SQLStatementCache {
    pub(super) fn get(&self, sql: &str) -> Option<CachedSQLStatement> {
        self.entries.get(sql).cloned()
    }

    pub(super) fn insert(
        &mut self,
        sql: String,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Arc<uqa_planner::UnifiedPlan>,
        catalog_epochs: CatalogEpochs,
    ) {
        let cached = CachedSQLStatement {
            statement,
            logical_plan,
            optimized_plan: None,
            catalog_epochs,
        };
        if let Entry::Occupied(mut entry) = self.entries.entry(sql.clone()) {
            entry.insert(cached);
            return;
        }
        while self.entries.len() >= SQL_STATEMENT_CACHE_LIMIT {
            let Some(oldest) = self.insertion_order.pop_front() else {
                self.entries.clear();
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                break;
            }
        }
        self.insertion_order.push_back(sql.clone());
        self.entries.insert(sql, cached);
    }

    pub(super) fn set_optimized(
        &mut self,
        sql: &str,
        optimized_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        if let Some(entry) = self.entries.get_mut(sql) {
            entry.optimized_plan = Some(optimized_plan);
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }
}

impl Engine {
    pub(crate) fn cached_sql_statement(&self, sql: &str) -> Option<CachedSQLStatement> {
        let cached = self.session.state.read().sql_statement_cache.get(sql)?;
        (cached.catalog_epochs == self.catalog_epochs()).then_some(cached)
    }

    pub(crate) fn cached_optimized_sql_plan(
        &self,
        sql: &str,
    ) -> Option<Arc<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql)?.optimized_plan
    }

    pub(crate) fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        let epochs = self.catalog_epochs();
        self.session
            .state
            .write()
            .sql_statement_cache
            .insert(sql, statement, logical_plan, epochs);
    }

    pub(crate) fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        self.session
            .state
            .write()
            .sql_statement_cache
            .set_optimized(sql, optimized_plan);
    }

    #[cfg(test)]
    pub(crate) fn cached_sql_plans(&self, sql: &str) -> Option<Vec<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql)
            .map(|cached| vec![cached.logical_plan.as_ref().clone()])
    }

    pub(crate) fn clear_sql_statement_cache(&self) {
        self.session.state.write().sql_statement_cache.clear();
    }
}
