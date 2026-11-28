//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned parsed, logical, and optimized SQL statement cache.

use std::sync::{Arc, OnceLock};
use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::memory::{BudgetedSharedMap, MemoryBudget, Produced, ProductionControl};
use uqa_sql::plan::SharedExecutablePlan;
use uqa_sql::SQLError;

use super::capabilities::CatalogEpochs;
use super::Engine;

const SQL_STATEMENT_CACHE_LIMIT: usize = 256;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct SQLStatementCache {
    checkpoint: StatementCacheCheckpoint,
    ordinary_memory: OnceLock<MemoryBudget>,
}

/// A transaction or routine retains the map roots. Each entry keeps its original reservation and a successor changes only its search paths.
#[derive(Clone, Default)]
pub(super) struct StatementCacheCheckpoint(Option<CacheState>);

#[derive(Clone)]
struct CacheState {
    entries: BudgetedSharedMap<CacheKey, CachedSQLStatement>,
    insertion_order: BudgetedSharedMap<u64, CacheKey>,
    optimized: BudgetedSharedMap<CacheKey, (SharedExecutablePlan, u64)>,
    next_insertion: u64,
}

#[derive(Clone)]
struct CacheKey(Arc<Produced<String>>);
impl Borrow<str> for CacheKey {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}
impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}
impl Eq for CacheKey {}
impl PartialOrd for CacheKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for CacheKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_str().cmp(other.0.as_str())
    }
}

#[derive(Clone)]
pub(crate) struct CachedSQLStatement {
    pub(crate) statement: Arc<uqa_sql::ast::Statement>,
    pub(crate) logical_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
    pub(crate) optimized_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
    pub(crate) analyzed_plan: Option<uqa_sql::binding::statements::AnalyzedStatement>,
    catalog_epochs: CatalogEpochs,
    function_registry_revision: u64,
    pub(crate) parser: uqa_sql::parser::ParserMetadata,
}

pub(super) struct SQLStatementInput {
    statement: Arc<uqa_sql::ast::Statement>,
    logical_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
    catalog_epochs: CatalogEpochs,
    function_registry_revision: u64,
    parser: uqa_sql::parser::ParserMetadata,
}

pub(crate) use uqa_sql::catalog::session::PreparedStatementMetadata;
pub(super) use uqa_sql::prepared::entry::PreparedStatementPlan;

impl StatementCacheCheckpoint {
    fn get(&self, sql: &str) -> Option<CachedSQLStatement> {
        let state = self.0.as_ref()?;
        let mut entry = state.entries.get(sql)?.clone();
        if let Some((plan, epoch)) = state.optimized.get(sql) {
            entry.optimized_plan = Some(plan.clone());
            entry.catalog_epochs.table_data = *epoch;
        }
        Some(entry)
    }
}

impl SQLStatementCache {
    pub(super) fn checkpoint(&self) -> StatementCacheCheckpoint {
        self.checkpoint.clone()
    }

    pub(super) fn restore(&mut self, checkpoint: &StatementCacheCheckpoint) {
        self.checkpoint.clone_from(checkpoint);
    }

    pub(super) fn get(&self, sql: &str) -> Option<CachedSQLStatement> {
        self.checkpoint.get(sql)
    }

    pub(super) fn insert(
        &mut self,
        sql: &str,
        input: SQLStatementInput,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let memory = mutation_budget(&self.ordinary_memory, control);
        if let Some(state) = &mut self.checkpoint.0 {
            return state.insert(sql, input, control, memory);
        }
        let mut state = CacheState::new(memory);
        state.insert(sql, input, control, memory)?;
        self.checkpoint.0 = Some(state);
        Ok(())
    }

    pub(super) fn set_optimized(
        &mut self,
        sql: &str,
        optimized_plan: SharedExecutablePlan,
        data_epoch: u64,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let Some(state) = &mut self.checkpoint.0 else {
            return Ok(());
        };
        let Some((key, _)) = state.entries.get_key_value(sql) else {
            return Ok(());
        };
        control.check()?;
        let memory = mutation_budget(&self.ordinary_memory, control);
        state
            .optimized
            .prepare_insert_with_budget(key.clone(), memory)?
            .publish((optimized_plan, data_epoch));
        Ok(())
    }

    fn update_statement(
        &mut self,
        sql: &str,
        control: &ProductionControl<'_>,
        replacement: impl FnOnce(&CachedSQLStatement) -> Option<CachedSQLStatement>,
    ) -> Result<(), SQLError> {
        let Some(state) = &mut self.checkpoint.0 else {
            return Ok(());
        };
        let Some((key, entry)) = state.entries.get_key_value(sql) else {
            return Ok(());
        };
        let Some(replacement) = replacement(entry) else {
            return Ok(());
        };
        control.check()?;
        let key = key.clone();
        let memory = mutation_budget(&self.ordinary_memory, control);
        state
            .entries
            .prepare_insert_with_budget(key, memory)?
            .publish(replacement);
        Ok(())
    }

    pub(super) fn set_analysis(
        &mut self,
        sql: &str,
        analysis: Option<uqa_sql::binding::statements::AnalyzedStatement>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.update_statement(sql, control, |entry| {
            Some(CachedSQLStatement {
                analyzed_plan: analysis,
                ..entry.clone()
            })
        })
    }

    pub(super) fn retire_logical(
        &mut self,
        sql: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.update_statement(sql, control, |entry| {
            entry.logical_plan.as_ref()?;
            Some(CachedSQLStatement {
                logical_plan: None,
                ..entry.clone()
            })
        })
    }

    pub(super) fn invalidate_optimized(&mut self) {
        if let Some(state) = &mut self.checkpoint.0 {
            state.optimized = BudgetedSharedMap::new(state.optimized.budget());
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

fn mutation_budget<'a>(
    ordinary: &'a OnceLock<MemoryBudget>,
    control: &ProductionControl<'a>,
) -> &'a MemoryBudget {
    control
        .budget()
        .unwrap_or_else(|| ordinary.get_or_init(|| MemoryBudget::new(usize::MAX)))
}

impl CacheState {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            entries: BudgetedSharedMap::new(memory),
            insertion_order: BudgetedSharedMap::new(memory),
            optimized: BudgetedSharedMap::new(memory),
            next_insertion: 0,
        }
    }

    fn insert(
        &mut self,
        sql: &str,
        input: SQLStatementInput,
        control: &ProductionControl<'_>,
        memory: &MemoryBudget,
    ) -> Result<(), SQLError> {
        let SQLStatementInput {
            statement,
            logical_plan,
            catalog_epochs,
            function_registry_revision,
            parser,
        } = input;
        let existing = self.entries.get_key_value(sql);
        let analyzed_plan = existing
            .filter(|(_, entry)| {
                Arc::ptr_eq(&entry.statement, &statement)
                    && entry.catalog_epochs.table_catalog == catalog_epochs.table_catalog
                    && entry.catalog_epochs.catalog_registry == catalog_epochs.catalog_registry
                    && entry.function_registry_revision == function_registry_revision
                    && entry.parser.settings == parser.settings
            })
            .and_then(|(_, entry)| entry.analyzed_plan.clone());
        let cached = CachedSQLStatement {
            statement,
            logical_plan: if analyzed_plan.is_some() {
                None
            } else {
                logical_plan
            },
            optimized_plan: None,
            analyzed_plan,
            catalog_epochs,
            function_registry_revision,
            parser,
        };

        if let Some((key, _)) = existing {
            let key = key.clone();
            let optimized = self
                .optimized
                .clone()
                .into_mutation_budget(memory)
                .with_remove(sql)?;
            self.entries
                .prepare_insert_with_budget(key, memory)?
                .publish(cached);
            self.optimized = optimized;
            return Ok(());
        }

        let next = self.next_insertion.checked_add(1).ok_or_else(|| {
            SQLError::Internal("statement cache insertion identity overflow".into())
        })?;
        let key = CacheKey(control.copy_text(sql)?.into_shared(control)?);
        if self.entries.len() < SQL_STATEMENT_CACHE_LIMIT {
            return self.append(key, cached, next, memory);
        }
        let (position, oldest) = self
            .insertion_order
            .first_from::<u64>(std::ops::Bound::Unbounded)
            .expect("cached statement has insertion order");
        let mut successor = Self {
            entries: self
                .entries
                .clone()
                .into_mutation_budget(memory)
                .with_remove(oldest)?,
            insertion_order: self
                .insertion_order
                .clone()
                .into_mutation_budget(memory)
                .with_remove(position)?,
            optimized: self
                .optimized
                .clone()
                .into_mutation_budget(memory)
                .with_remove(oldest)?,
            next_insertion: self.next_insertion,
        };
        successor.append(key, cached, next, memory)?;
        *self = successor;
        Ok(())
    }

    fn append(
        &mut self,
        key: CacheKey,
        entry: CachedSQLStatement,
        next: u64,
        memory: &MemoryBudget,
    ) -> Result<(), SQLError> {
        // Both indexes admit their complete updates before either one is published.
        let order = self
            .insertion_order
            .prepare_insert_with_budget(self.next_insertion, memory)?;
        let statement = self
            .entries
            .prepare_insert_with_budget(key.clone(), memory)?;
        order.publish(key);
        statement.publish(entry);
        self.next_insertion = next;
        Ok(())
    }
}

impl Engine {
    pub(crate) fn cached_sql_analysis(
        &self,
        sql: &str,
    ) -> Option<uqa_sql::binding::statements::AnalyzedStatement> {
        if self.analysis_catalog_is_dirty() {
            return None;
        }
        self.cached_sql_statement(sql)?.analyzed_plan
    }

    pub(crate) fn cache_sql_analysis(
        &self,
        sql: &str,
        analysis: Option<uqa_sql::binding::statements::AnalyzedStatement>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if !self.analysis_catalog_is_dirty() {
            self.session
                .state
                .write()
                .sql_statement_cache
                .set_analysis(sql, analysis, control)?;
        }
        Ok(())
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
            || cached.function_registry_revision != self.extensions.functions.revision()
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
    ) -> Option<uqa_sql::plan::SharedExecutablePlan> {
        self.cached_sql_statement(sql)?.optimized_plan
    }

    pub(crate) fn cache_sql_statement(
        &self,
        sql: &str,
        statement: Arc<uqa_sql::ast::Statement>,
        logical_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
        parser: uqa_sql::parser::ParserMetadata,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let epochs = self.catalog_epochs();
        self.session.state.write().sql_statement_cache.insert(
            sql,
            SQLStatementInput {
                statement,
                logical_plan,
                catalog_epochs: epochs,
                function_registry_revision: self.extensions.functions.revision(),
                parser,
            },
            control,
        )
    }

    pub(crate) fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: uqa_sql::plan::SharedExecutablePlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let data_epoch = self.catalog_epochs().table_data;
        self.session
            .state
            .write()
            .sql_statement_cache
            .set_optimized(sql, optimized_plan, data_epoch, control)
    }

    #[cfg(test)]
    pub(crate) fn cached_sql_plans(&self, sql: &str) -> Option<Vec<uqa_planner::UnifiedPlan>> {
        self.cached_sql_statement(sql).and_then(|cached| {
            cached
                .logical_plan
                .as_ref()
                .map(|plan| plan.as_ref().clone())
                .or_else(|| {
                    cached
                        .analyzed_plan
                        .as_ref()
                        .and_then(|plan| plan.plan_for(&[]))
                        .cloned()
                })
                .map(|plan| vec![plan])
        })
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
