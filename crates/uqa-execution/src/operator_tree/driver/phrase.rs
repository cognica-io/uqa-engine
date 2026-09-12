//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained query revisions and SQL runtime controls for native phrase execution.

use super::{
    operator_execution_error, DriverResult, PhysicalRetrievalDriver, PostingList, SQLError,
    ScoredEntry, TextScoringMode,
};
use uqa_operators::phrase::{score_phrase, PhraseBudget, PhraseError};
use uqa_scoring::{BM25Params, ScoringMode};

impl PhysicalRetrievalDriver<'_> {
    pub(super) fn execute_phrase(
        &self,
        query: &str,
        field: Option<&str>,
        scoring: Option<TextScoringMode>,
    ) -> DriverResult<PostingList> {
        self.execute_phrase_counted(query, field, scoring)
            .map(|(rows, _)| rows)
    }

    pub(super) fn execute_phrase_counted(
        &self,
        query: &str,
        field: Option<&str>,
        scoring: Option<TextScoringMode>,
    ) -> DriverResult<(PostingList, usize)> {
        let scoring = scoring.ok_or_else(|| {
            SQLError::Internal("phrase reached execution without bound text scoring".into())
        })?;
        if field.is_none()
            && matches!(
                scoring,
                TextScoringMode::CustomBM25(_) | TextScoringMode::CustomBayesianBM25(_)
            )
        {
            return Err(SQLError::TypeMismatch(
                "explicit text scoring parameters require one concrete field".into(),
            ));
        }
        let fields = match field {
            Some(field) => vec![field.to_owned()],
            None => self.context.text.fts_fields_for_table(self.table)?,
        };
        if fields.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "text search: table `{}` has no text-indexed columns",
                self.table
            )));
        }
        let mut rows = Vec::new();
        let mut query_units = 0;
        let runtime = self.context.runtime;
        let mut budget = PhraseBudget::new(runtime.work_mem_bytes()?, runtime.cancellation);
        for field in fields {
            self.context
                .text
                .validate_text_search_field(self.table, &field)?;
            let mode = match scoring {
                TextScoringMode::BM25 => ScoringMode::BM25(BM25Params::default()),
                TextScoringMode::BayesianBM25 => {
                    ScoringMode::BayesianBM25(self.bayesian_params_for(&field)?)
                }
                TextScoringMode::CustomBM25(params) => ScoringMode::BM25(params),
                TextScoringMode::CustomBayesianBM25(params) => ScoringMode::BayesianBM25(params),
            };
            let (matches, units) = self.phrase_field(query, &field, &mode, &mut budget)?;
            query_units = query_units.max(units);
            budget
                .append_results(&mut rows, matches)
                .map_err(phrase_sql_error)?;
        }
        budget
            .finish_postings(rows)
            .map(|rows| (rows, query_units))
            .map_err(phrase_sql_error)
    }

    fn phrase_field(
        &self,
        query: &str,
        field: &str,
        mode: &ScoringMode,
        budget: &mut PhraseBudget<'_>,
    ) -> DriverResult<(Vec<ScoredEntry>, usize)> {
        budget.check_cancelled().map_err(phrase_sql_error)?;
        let state = self
            .context
            .indexes
            .query_table_indexes(self.table)
            .map_err(|error| operator_execution_error("resolve phrase index", error))?
            .ok_or_else(|| SQLError::UnknownTable(self.table.into()))?;
        let index = state.inverted_index();
        let index = index.as_ref().as_ref();
        let revision = index
            .search_analyzer_revision(field)
            .map_err(|error| operator_execution_error("resolve phrase analyzer revision", error))?;
        let graph = uqa_storage::inverted_index::analyze_query_graph(&revision, query)
            .map_err(|error| operator_execution_error("analyze phrase", error))?;
        score_phrase(index, field, &graph, mode, budget)
            .map(|rows| (rows, graph.len()))
            .map_err(phrase_sql_error)
    }
}

fn phrase_sql_error(error: PhraseError) -> SQLError {
    match error {
        PhraseError::Cancelled(error) => SQLError::Cancelled(error),
        error @ (PhraseError::MemoryLimit { .. } | PhraseError::Allocation(_)) => {
            SQLError::Routine {
                sqlstate: "53200".into(),
                message: error.to_string(),
            }
        }
        PhraseError::Scoring(uqa_scoring::TextSearchError::Parameters(error)) => {
            SQLError::Routine {
                sqlstate: "22023".into(),
                message: error.to_string(),
            }
        }
        error => operator_execution_error("phrase", error),
    }
}
