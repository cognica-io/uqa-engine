//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned parsed, logical, and optimized SQL statement cache.

use std::collections::{btree_map::Entry, BTreeMap, VecDeque};
use std::sync::Arc;

use super::capabilities::CatalogEpochs;
use super::Engine;

const SQL_STATEMENT_CACHE_LIMIT: usize = 256;

#[derive(Clone, Default)]
pub(super) struct SQLStatementCache {
    entries: BTreeMap<String, CachedSQLStatement>,
    insertion_order: VecDeque<String>,
    has_optimized: bool,
}

#[derive(Clone)]
pub(crate) struct CachedSQLStatement {
    pub(crate) statement: Arc<uqa_sql::ast::Statement>,
    pub(crate) logical_plan: Arc<uqa_planner::UnifiedPlan>,
    pub(crate) optimized_plan: Option<Arc<uqa_planner::UnifiedPlan>>,
    pub(crate) analyzed_plan: Option<Arc<uqa_sql::binding::statements::AnalyzedStatement>>,
    catalog_epochs: CatalogEpochs,
    pub(crate) parser: uqa_sql::parser::ParserMetadata,
}

pub(crate) use uqa_sql::catalog::session::PreparedStatementMetadata;
pub(super) use uqa_sql::prepared::entry::PreparedStatementPlan;

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
        parser: uqa_sql::parser::ParserMetadata,
    ) {
        let analyzed_plan = self
            .entries
            .get(&sql)
            .filter(|entry| {
                Arc::ptr_eq(&entry.statement, &statement)
                    && entry.catalog_epochs.table_catalog == catalog_epochs.table_catalog
                    && entry.catalog_epochs.catalog_registry == catalog_epochs.catalog_registry
                    && entry.parser.settings == parser.settings
            })
            .and_then(|entry| entry.analyzed_plan.clone());
        let cached = CachedSQLStatement {
            statement,
            logical_plan,
            optimized_plan: None,
            analyzed_plan,
            catalog_epochs,
            parser,
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
        data_epoch: u64,
    ) {
        if let Some(entry) = self.entries.get_mut(sql) {
            entry.optimized_plan = Some(optimized_plan);
            entry.catalog_epochs.table_data = data_epoch;
            self.has_optimized = true;
        }
    }

    pub(super) fn invalidate_optimized(&mut self) {
        if !std::mem::take(&mut self.has_optimized) {
            return;
        }
        for entry in self.entries.values_mut() {
            entry.optimized_plan = None;
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
        self.has_optimized = false;
    }
}

impl Engine {
    pub(crate) fn cached_sql_analysis(
        &self,
        sql: &str,
    ) -> Option<Arc<uqa_sql::binding::statements::AnalyzedStatement>> {
        if self.analysis_catalog_is_dirty() {
            return None;
        }
        self.cached_sql_statement(sql)?.analyzed_plan
    }

    pub(crate) fn cache_sql_analysis(
        &self,
        sql: &str,
        analysis: Option<Arc<uqa_sql::binding::statements::AnalyzedStatement>>,
    ) {
        if !self.analysis_catalog_is_dirty() {
            if let Some(entry) = self
                .session
                .state
                .write()
                .sql_statement_cache
                .entries
                .get_mut(sql)
            {
                entry.analyzed_plan = analysis;
            }
        }
    }

    fn analysis_catalog_is_dirty(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.epochs.table_catalog.dirty.load(Ordering::Acquire)
            || self.epochs.catalog_registry.dirty.load(Ordering::Acquire)
    }

    pub(crate) fn cached_sql_statement(&self, sql: &str) -> Option<CachedSQLStatement> {
        let mut cached = self.session.state.read().sql_statement_cache.get(sql)?;
        let epochs = self.catalog_epochs();
        if cached.catalog_epochs.table_catalog != epochs.table_catalog
            || cached.catalog_epochs.catalog_registry != epochs.catalog_registry
            || cached.parser.settings != self.parser_settings()
        {
            return None;
        }
        // Structural lowering depends on definitions and parser settings;
        // data-dependent access paths must be chosen again after a data change.
        if cached.catalog_epochs.table_data != epochs.table_data {
            cached.optimized_plan = None;
        }
        Some(cached)
    }

    #[cfg(test)]
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
        parser: uqa_sql::parser::ParserMetadata,
    ) {
        let epochs = self.catalog_epochs();
        self.session.state.write().sql_statement_cache.insert(
            sql,
            statement,
            logical_plan,
            epochs,
            parser,
        );
    }

    pub(crate) fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: Arc<uqa_planner::UnifiedPlan>,
    ) {
        let data_epoch = self.catalog_epochs().table_data;
        self.session
            .state
            .write()
            .sql_statement_cache
            .set_optimized(sql, optimized_plan, data_epoch);
    }

    #[cfg(test)]
    pub(crate) fn cached_sql_plans(&self, sql: &str) -> Option<Vec<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql)
            .map(|cached| vec![cached.logical_plan.as_ref().clone()])
    }

    pub(crate) fn clear_sql_statement_cache(&self) {
        self.session.state.write().sql_statement_cache.clear();
    }

    pub(crate) fn invalidate_optimized_sql_plans(&self) {
        self.session
            .state
            .write()
            .sql_statement_cache
            .invalidate_optimized();
    }
}
