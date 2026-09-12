//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND/BMW planning, block-max lifecycle, and profiled text leaves.

use super::{
    storage_sql_error, Engine, OperatorTree, SQLError, ScoredEntry, ScoringMode, TextScoringMode,
    TextSearchAlgorithm, TextSearchProfile, TextTopKPlan, TextTopKStrategy,
};

impl Engine {
    pub(crate) fn plan_text_top_k_tree(
        &self,
        table: &str,
        field: &str,
        query: &str,
        scoring: TextScoringMode,
        top_k: usize,
    ) -> Result<OperatorTree, SQLError> {
        uqa_planner::retrieval_planning::plan_text_top_k_tree(
            self, table, field, query, scoring, top_k,
        )
    }

    /// Materialize scorer-versioned block bounds for one text field. `SQLite`
    /// persists them across reopen; when a backend returns `false`, execution
    /// falls back from the planned BMW strategy to exact WAND.
    pub fn rebuild_text_block_max(
        &self,
        table: &str,
        field: &str,
        mode: &ScoringMode,
    ) -> Result<bool, SQLError> {
        self.validate_text_search_field(table, field)?;
        self.with_implicit_transaction(|engine| {
            let Some(table_state) = engine
                .try_table(table)
                .map_err(|error| storage_sql_error("resolve text-search table", error))?
            else {
                return Err(SQLError::UnknownTable(table.to_string()));
            };
            let mut index = table_state.inverted_index.write();
            uqa_scoring::rebuild_text_block_max(index.as_mut(), field, mode)
                .map_err(super::helpers::scoring_sql_error)
        })
    }

    pub(super) fn search_leaf_profiled(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
        physical_top_k: Option<TextTopKPlan>,
    ) -> Result<TextSearchProfile, SQLError> {
        let table_state = self
            .try_query_table(table)
            .map_err(|error| storage_sql_error("resolve text-search table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let index = table_state.inverted_index.read();
        let (limit, strategy) = match physical_top_k {
            Some(plan) => (
                plan.k,
                match plan.strategy {
                    TextTopKStrategy::Wand => TextSearchAlgorithm::Wand,
                    TextTopKStrategy::BlockMaxWand => TextSearchAlgorithm::BlockMaxWand,
                },
            ),
            None => (top_k, TextSearchAlgorithm::Exhaustive),
        };
        uqa_scoring::score_text_query(index.as_ref(), table, field, query, mode, limit, strategy)
            .map_err(super::helpers::scoring_sql_error)
    }

    /// Physical text-search leaf. Only [`crate::operator_tree_bridge::EngineDriver`]
    /// calls this; public callers enter through [`Self::search`] below.
    pub(crate) fn search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
        physical_top_k: Option<TextTopKPlan>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        Ok(self
            .search_leaf_profiled(table, field, query, mode, top_k, physical_top_k)?
            .entries)
    }
}
