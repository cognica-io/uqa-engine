//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply retained table/index guards and graph snapshots to the retrieval planner.

use crate::{Engine, TableState};
use parking_lot::RwLockReadGuard;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{catalog_index::CatalogIndexRow, Predicate};
use uqa_planner::{
    retrieval_planning::{
        GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable,
        TextStatisticsRead, VectorStatisticsRead,
    },
    ColumnStats,
};
use uqa_sql::SQLError;
use uqa_storage::{InvertedIndex, VectorIndex};

struct TableStatistics(Arc<TableState>);
struct TextRead<'a>(RwLockReadGuard<'a, Box<dyn InvertedIndex>>);
struct VectorRead<'a>(RwLockReadGuard<'a, BTreeMap<String, Box<dyn VectorIndex>>>);
impl TextStatisticsRead for TextRead<'_> {
    fn analyze(&self, field: &str, query: &str) -> Result<Vec<String>, String> {
        self.0
            .get_search_analyzer(field)
            .analyze(query)
            .map_err(|error| error.to_string())
    }
    fn doc_freq(&self, field: &str, term: &str) -> Result<u64, String> {
        self.0
            .doc_freq(field, term)
            .map_err(|error| error.to_string())
    }
    fn doc_freq_any_field(&self, term: &str) -> Result<u64, String> {
        self.0
            .doc_freq_any_field(term)
            .map_err(|error| error.to_string())
    }
}
impl VectorStatisticsRead for VectorRead<'_> {
    fn dimensions(&self, field: &str) -> Option<u32> {
        self.0.get(field).map(|index| index.dimensions())
    }
}
impl RetrievalStatisticsTable for TableStatistics {
    fn text_index(&self) -> Box<dyn TextStatisticsRead + '_> {
        Box::new(TextRead(self.0.inverted_index.read()))
    }
    fn vector_indexes(&self) -> Box<dyn VectorStatisticsRead + '_> {
        Box::new(VectorRead(self.0.vector_indexes.read()))
    }
}
impl RetrievalPlanningCatalog for Engine {
    fn has_table(&self, table: &str) -> Result<bool, String> {
        self.has_table(table).map_err(|error| error.to_string())
    }
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String> {
        self.resolve_table_name(table)
            .map_err(|error| error.to_string())
    }
    fn list_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, String> {
        self.list_catalog_indexes()
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
    fn table_doc_count(&self, table: &str) -> Result<u64, SQLError> {
        self.table_doc_count(table)
    }
    fn try_query_table(
        &self,
        table: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        self.try_query_table(table)
            .map(|state| {
                state.map(|state| {
                    Box::new(TableStatistics(state)) as Box<dyn RetrievalStatisticsTable>
                })
            })
            .map_err(|error| error.to_string())
    }
    fn try_query_column_stats(&self, table: &str) -> Result<BTreeMap<String, ColumnStats>, String> {
        self.try_query_column_stats(table)
            .map_err(|error| error.to_string())
    }
    fn graph_snapshot(&self, graph: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        self.graph_with(graph, |store| {
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

#[cfg(test)]
mod tests;
