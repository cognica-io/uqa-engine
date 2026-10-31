//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply retained table/index guards and graph snapshots to the retrieval planner.

use crate::{Engine, TableState};
use parking_lot::RwLockReadGuard;
use std::sync::Arc;
use uqa_core::{catalog_index::CatalogIndexSnapshot, Predicate};
use uqa_planner::{
    retrieval_planning::{
        GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable,
        TextStatisticsRead, VectorStatisticsRead,
    },
    ColumnStatistics,
};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, InvertedIndex};

struct TableStatistics(Arc<TableState>, StorageReadControl);
struct TextRead<'a>(
    RwLockReadGuard<'a, Box<dyn InvertedIndex>>,
    &'a StorageReadControl,
);
struct VectorRead<'a>(
    RwLockReadGuard<'a, uqa_storage::vector_index::VectorIndexes>,
    &'a StorageReadControl,
);
impl TextStatisticsRead for TextRead<'_> {
    fn query_doc_freq_with_control(
        &self,
        field: Option<&str>,
        query: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<u64, SQLError> {
        control.check()?;
        let frequency =
            uqa_storage::inverted_index::query_document_frequency(&**self.0, field, query, self.1)
                .map_err(|error| {
                    crate::search::storage_sql_error("execute analyze optimizer query", error)
                })?;
        control.check()?;
        Ok(frequency)
    }

    fn field_names(&self) -> Result<Option<Vec<String>>, SQLError> {
        self.0.field_names().map(Some).map_err(|error| {
            crate::search::storage_sql_error("execute read document frequency", error)
        })
    }

    fn analyze_utf16(&self, field: &str, query: &str) -> Result<Vec<Vec<u16>>, SQLError> {
        Ok(self
            .0
            .search_analyzer_revision(field)
            .map_err(|error| {
                crate::search::storage_sql_error("execute analyze optimizer query", error)
            })?
            .analyze_tokens(query)
            .map_err(analysis_error)?
            .tokens()
            .iter()
            .map(|token| token.term().utf16().into_owned())
            .collect())
    }
    fn doc_freq_utf16(&self, field: &str, term: &[u16]) -> Result<u64, SQLError> {
        let key = uqa_storage::TokenTermKey::from_term(&uqa_analysis::TokenTerm::from_utf16(
            term.to_vec(),
        ));
        self.0.doc_freq_key(field, &key).map_err(|error| {
            crate::search::storage_sql_error("execute read document frequency", error)
        })
    }

    fn analyze(&self, field: &str, query: &str) -> Result<Vec<String>, SQLError> {
        self.0
            .search_analyzer_revision(field)
            .map_err(|error| {
                crate::search::storage_sql_error("execute analyze optimizer query", error)
            })?
            .analyze(query)
            .map_err(analysis_error)
    }
    fn doc_freq(&self, field: &str, term: &str) -> Result<u64, SQLError> {
        self.0.doc_freq(field, term).map_err(|error| {
            crate::search::storage_sql_error("execute read document frequency", error)
        })
    }
    fn doc_freq_any_field(&self, term: &str) -> Result<u64, SQLError> {
        self.0.doc_freq_any_field(term).map_err(|error| {
            crate::search::storage_sql_error("execute read document frequency", error)
        })
    }
}
fn analysis_error(error: uqa_analysis::AnalysisError) -> SQLError {
    match error {
        uqa_analysis::AnalysisError::Memory(error) => {
            crate::search::storage_sql_error("execute analyze optimizer query", error)
        }
        uqa_analysis::AnalysisError::Cancelled => {
            SQLError::Cancelled(uqa_core::QueryCancelled::USER_REQUEST)
        }
        error => SQLError::Internal(format!("execute analyze optimizer query: {error}")),
    }
}
impl VectorStatisticsRead for VectorRead<'_> {
    fn dimensions(&self, field: &str) -> Option<u32> {
        self.0.get(field).map(uqa_storage::VectorIndex::dimensions)
    }
    fn diskann_index_statistics(
        &self,
        field: &str,
    ) -> Result<Option<uqa_core::DiskANNIndexStats>, SQLError> {
        let Some(index) = self.0.get(field) else {
            return Ok(None);
        };
        index
            .diskann_query_metadata(self.1)
            .and_then(|metadata| {
                metadata
                    .map(|metadata| metadata.index_statistics(self.1))
                    .transpose()
            })
            .map_err(|error| {
                crate::search::storage_sql_error("read physical vector statistics", error)
            })
    }
    fn diskann_query_statistics(
        &self,
        field: &str,
        query: &[f32],
    ) -> Result<Option<uqa_core::DiskANNQueryStats>, SQLError> {
        let Some(index) = self.0.get(field) else {
            return Ok(None);
        };
        index
            .diskann_query_metadata(self.1)
            .and_then(|metadata| {
                metadata
                    .map(|metadata| metadata.query_statistics(query, self.1))
                    .transpose()
            })
            .map_err(|error| {
                crate::search::storage_sql_error("read physical vector statistics", error)
            })
    }
}
impl RetrievalStatisticsTable for TableStatistics {
    fn text_index(&self) -> Box<dyn TextStatisticsRead + '_> {
        Box::new(TextRead(self.0.inverted_index.read(), &self.1))
    }
    fn vector_indexes(&self) -> Box<dyn VectorStatisticsRead + '_> {
        Box::new(VectorRead(self.0.vector_indexes.read(), &self.1))
    }
}
impl RetrievalPlanningCatalog for Engine {
    fn production_resources(&self) -> Result<Option<uqa_planner::PlanningResources>, SQLError> {
        uqa_planner::statement_planning::executable::StatementOptimizationContexts::production_resources(self)
    }

    fn has_table(&self, table: &str) -> Result<bool, String> {
        self.has_table_in_execution(table)
            .map_err(|error| error.to_string())
    }
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String> {
        self.resolve_table_name(table)
            .map_err(|error| error.to_string())
    }
    fn list_catalog_indexes(&self) -> Result<CatalogIndexSnapshot, String> {
        self.catalog_index_snapshot_in_execution()
            .map_err(|error| error.to_string())
    }
    fn value_index_cardinality(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        self.value_index_cardinality(table, field, predicate)
    }
    fn value_index_supports(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<bool, String> {
        self.value_index_supports(table, field, predicate)
            .map_err(|error| error.to_string())
    }
    fn text_top_k_capabilities(
        &self,
        table: &str,
        field: &str,
        query: &str,
    ) -> Result<uqa_planner::TextTopKCapabilities, SQLError> {
        let Some(t) = self.try_query_table(table).map_err(|error| {
            crate::search::storage_sql_error("resolve text-search table", error)
        })?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let index = t.inverted_index.read();
        let control = self.query_retention_control()?;
        let analyzed_terms =
            uqa_storage::inverted_index::query_term_count(&**index, field, query, &control)
                .map_err(|error| crate::search::storage_sql_error("analyze text query", error))?;
        let indexed_document_count = index
            .field_stats_scalar_budgeted(field, &control)
            .map_err(|error| {
                crate::search::storage_sql_error("read indexed document count", error)
            })?
            .total_docs;
        Ok(uqa_planner::TextTopKCapabilities {
            analyzed_term_count: analyzed_terms,
            indexed_document_count,
        })
    }

    fn table_doc_count(&self, table: &str) -> Result<u64, SQLError> {
        self.table_row_estimate(table)
    }
    fn try_query_table(
        &self,
        table: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        let Some(state) = self
            .try_query_table(table)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let control = self
            .query_retention_control()
            .map_err(|error| error.to_string())?;
        Ok(Some(Box::new(TableStatistics(state, control))))
    }
    fn try_query_column_stats(&self, table: &str) -> Result<ColumnStatistics, String> {
        self.try_query_column_stats(table)
            .map_err(|error| error.to_string())
    }
    fn graph_snapshot(&self, graph: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        self.graph_statistics_with(graph, |store| {
            use uqa_graph::GraphStore as _;
            let vertices = store.vertices_in_graph(graph)?;
            let edges = store.edges_in_graph(graph)?;
            let degree_distribution = store.degree_distribution(graph)?;
            let vertex_label_counts = store.vertex_label_counts(graph)?;
            Ok::<_, uqa_graph::GraphStoreError>(GraphStatisticsSnapshot {
                vertices,
                edges,
                degree_distribution,
                vertex_label_counts,
            })
        })
        .map_err(|error| error.to_string())?
        .transpose()
        .map_err(|error| error.to_string())
    }
}

impl uqa_execution::operator_tree::query::RelationRetrievalPlanner for Engine {
    fn accelerated_tree(
        &self,
        table: &str,
        expression: &uqa_sql::ScalarExpr,
        tree: uqa_operators::OperatorTree,
    ) -> Result<Option<uqa_operators::OperatorTree>, SQLError> {
        uqa_planner::retrieval_planning::accelerated_tree(self, table, expression, tree)
    }
    fn text_top_k(
        &self,
        table: &str,
        tree: uqa_operators::OperatorTree,
        top_k: usize,
    ) -> Result<uqa_operators::OperatorTree, SQLError> {
        uqa_planner::retrieval_planning::plan_bound_text_top_k(self, table, tree, top_k)
    }
}
