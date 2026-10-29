//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned parsed, logical, and optimized SQL statement cache.

use std::sync::Arc;
use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::memory::{MemoryReservation, OwnedMap, Produced, ProductionControl};
use uqa_sql::SQLError;

use super::capabilities::CatalogEpochs;
use super::Engine;

const SQL_STATEMENT_CACHE_LIMIT: usize = 256;

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub(super) struct SQLStatementCache {
    state: Option<Arc<CacheState>>,
    optimization_generation: u64,
    has_optimized: bool,
}

struct CacheState {
    entries: OwnedMap<CacheKey, CachedSQLStatement>,
    insertion_order: OwnedMap<u64, CacheKey>,
    next_insertion: u64,
    _memory: Option<MemoryReservation>,
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
    pub(crate) logical_plan: uqa_sql::plan::SharedExecutablePlan,
    pub(crate) optimized_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
    pub(crate) analyzed_plan: Option<uqa_sql::binding::statements::AnalyzedStatement>,
    catalog_epochs: CatalogEpochs,
    function_registry_revision: u64,
    pub(crate) parser: uqa_sql::parser::ParserMetadata,
    optimization_generation: u64,
}

pub(super) struct SQLStatementInput {
    statement: Arc<uqa_sql::ast::Statement>,
    logical_plan: uqa_sql::plan::SharedExecutablePlan,
    catalog_epochs: CatalogEpochs,
    function_registry_revision: u64,
    parser: uqa_sql::parser::ParserMetadata,
}

pub(crate) use uqa_sql::catalog::session::PreparedStatementMetadata;
pub(super) use uqa_sql::prepared::entry::PreparedStatementPlan;

impl SQLStatementCache {
    pub(super) fn get(&self, sql: &str) -> Option<CachedSQLStatement> {
        let mut entry = self.state.as_ref()?.entries.get(sql)?.clone();
        if entry.optimization_generation != self.optimization_generation {
            entry.optimized_plan = None;
        }
        Some(entry)
    }

    /// Snapshots share their admitted allocation. A write copies shared nodes under
    /// the invoking request before publishing any change; key and plan payloads
    /// retain their original owners. Rollback only swaps existing shared handles.
    fn update(
        &mut self,
        control: &ProductionControl<'_>,
        update: impl FnOnce(&mut CacheState) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(state) = self.state.as_mut().and_then(Arc::get_mut) {
            return update(state);
        }
        let mut state = CacheState::new(control)?;
        if let Some(source) = &self.state {
            state.next_insertion = source.next_insertion;
            for (key, value) in &source.entries {
                let mut value = value.clone();
                if value.optimization_generation != self.optimization_generation {
                    value.optimized_plan = None;
                }
                insert(&mut state.entries, key.clone(), value, control)?;
            }
            for (position, key) in &source.insertion_order {
                insert(&mut state.insertion_order, *position, key.clone(), control)?;
            }
        }
        update(&mut state)?;
        self.state = Some(Arc::new(state));
        Ok(())
    }

    pub(super) fn insert(
        &mut self,
        sql: &str,
        input: SQLStatementInput,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let generation = self.optimization_generation;
        self.update(control, |state| {
            state.insert(sql, input, generation, control)
        })
    }

    pub(super) fn set_optimized(
        &mut self,
        sql: &str,
        optimized_plan: uqa_sql::plan::SharedExecutablePlan,
        data_epoch: u64,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self
            .state
            .as_ref()
            .is_none_or(|state| !state.entries.contains_key(sql))
        {
            return Ok(());
        }
        let generation = self.optimization_generation;
        self.update(control, |state| {
            let entry = state.entries.get_mut(sql).expect("selected cache entry");
            entry.optimized_plan = Some(optimized_plan);
            entry.optimization_generation = generation;
            entry.catalog_epochs.table_data = data_epoch;
            Ok(())
        })?;
        self.has_optimized = true;
        Ok(())
    }

    pub(super) fn set_analysis(
        &mut self,
        sql: &str,
        analysis: Option<uqa_sql::binding::statements::AnalyzedStatement>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self
            .state
            .as_ref()
            .is_none_or(|state| !state.entries.contains_key(sql))
        {
            return Ok(());
        }
        self.update(control, |state| {
            state
                .entries
                .get_mut(sql)
                .expect("selected cache entry")
                .analyzed_plan = analysis;
            Ok(())
        })
    }

    pub(super) fn invalidate_optimized(&mut self) {
        if !std::mem::take(&mut self.has_optimized) {
            return;
        }
        if let Some(state) = self.state.as_mut().and_then(Arc::get_mut) {
            for entry in state.entries.values_mut() {
                entry.optimized_plan = None;
            }
        } else if let Some(generation) = self.optimization_generation.checked_add(1) {
            self.optimization_generation = generation;
        } else {
            self.clear();
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

impl CacheState {
    fn new(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Ok(Self {
            entries: OwnedMap::new(),
            insertion_order: OwnedMap::new(),
            next_insertion: 0,
            _memory: control.reserve(size_of::<Self>())?,
        })
    }

    fn insert(
        &mut self,
        sql: &str,
        input: SQLStatementInput,
        optimization_generation: u64,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let SQLStatementInput {
            statement,
            logical_plan,
            catalog_epochs,
            function_registry_revision,
            parser,
        } = input;
        control.check()?;
        let analyzed_plan = self
            .entries
            .get(sql)
            .filter(|entry| {
                Arc::ptr_eq(&entry.statement, &statement)
                    && entry.catalog_epochs.table_catalog == catalog_epochs.table_catalog
                    && entry.catalog_epochs.catalog_registry == catalog_epochs.catalog_registry
                    && entry.function_registry_revision == function_registry_revision
                    && entry.parser.settings == parser.settings
            })
            .and_then(|entry| entry.analyzed_plan.clone());
        let cached = CachedSQLStatement {
            statement,
            logical_plan,
            optimized_plan: None,
            analyzed_plan,
            catalog_epochs,
            function_registry_revision,
            parser,
            optimization_generation,
        };
        if let Some(entry) = self.entries.get_mut(sql) {
            *entry = cached;
            return Ok(());
        }
        let next = self.next_insertion.checked_add(1).ok_or_else(|| {
            SQLError::Internal("statement cache insertion identity overflow".into())
        })?;
        let key = CacheKey(control.copy_text(sql)?.into_shared(control)?);
        insert(
            &mut self.insertion_order,
            self.next_insertion,
            key.clone(),
            control,
        )?;
        if let Err(error) = insert(&mut self.entries, key, cached, control) {
            self.insertion_order.remove(&self.next_insertion);
            return Err(error);
        }
        self.next_insertion = next;
        while self.entries.len() > SQL_STATEMENT_CACHE_LIMIT {
            let (position, key) = self
                .insertion_order
                .iter()
                .next()
                .map(|(position, key)| (*position, key.clone()))
                .expect("cached statement has insertion order");
            self.insertion_order.remove(&position);
            self.entries.remove(key.0.as_str());
        }
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
        logical_plan: uqa_sql::plan::SharedExecutablePlan,
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

fn insert<K: Ord, V>(
    map: &mut OwnedMap<K, V>,
    key: K,
    value: V,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        map.insert_budgeted(key, value, budget)?;
    } else {
        map.insert(key, value);
    }
    Ok(())
}
