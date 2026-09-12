//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only catalog and retained index interfaces for retrieval costing.

use crate::ColumnStats;
use std::collections::BTreeMap;
use uqa_core::{catalog_index::CatalogIndexRow, Edge, Predicate, Vertex};
use uqa_sql::SQLError;

/// The implementation retains the actual text-index read guard through all analyzer and frequency reads.
pub trait TextStatisticsRead {
    fn analyze(&self, field: &str, query: &str) -> Result<Vec<String>, String>;
    fn doc_freq(&self, field: &str, term: &str) -> Result<u64, String>;
    fn doc_freq_any_field(&self, term: &str) -> Result<u64, String>;
    /// Field inventory for analyzed all-field queries. Legacy scalar adapters may omit it.
    fn field_names(&self) -> Result<Option<Vec<String>>, String> {
        Ok(None)
    }
    /// Estimate all-field query support using each field's retained search revision.
    fn query_doc_freq_any_field(&self, query: &str) -> Result<u64, String> {
        let Some(fields) = self.field_names()? else {
            return self.doc_freq_any_field(query);
        };
        let mut frequency = 0_u64;
        for field in fields {
            for term in self.analyze_utf16(&field, query)? {
                frequency = frequency.saturating_add(self.doc_freq_utf16(&field, &term)?);
            }
        }
        Ok(frequency)
    }

    fn analyze_utf16(&self, field: &str, query: &str) -> Result<Vec<Vec<u16>>, String> {
        Ok(self
            .analyze(field, query)?
            .into_iter()
            .map(|term| term.encode_utf16().collect())
            .collect())
    }
    fn doc_freq_utf16(&self, field: &str, term: &[u16]) -> Result<u64, String> {
        self.doc_freq(
            field,
            &String::from_utf16(term).map_err(|error| error.to_string())?,
        )
    }
}
/// The implementation retains the actual vector-index registry read guard.
pub trait VectorStatisticsRead {
    fn dimensions(&self, field: &str) -> Option<u32>;
}
/// Retain one table generation across text and vector index reads.
pub trait RetrievalStatisticsTable {
    fn text_index(&self) -> Box<dyn TextStatisticsRead + '_>;
    fn vector_indexes(&self) -> Box<dyn VectorStatisticsRead + '_>;
}
pub struct GraphStatisticsSnapshot {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    pub degree_distribution: BTreeMap<u64, u64>,
    pub vertex_label_counts: BTreeMap<String, u64>,
}
pub trait RetrievalPlanningCatalog {
    fn has_table(&self, table: &str) -> Result<bool, String>;
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String>;
    fn list_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, String>;
    fn value_index_cardinality(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<usize>, SQLError>;
    fn value_index_supports(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<bool, String>;
    /// Read analyzed term and indexed-document counts under one retained text-index guard.
    fn text_top_k_capabilities(
        &self,
        table: &str,
        field: &str,
        query: &str,
    ) -> Result<crate::TextTopKCapabilities, SQLError>;
    fn table_doc_count(&self, table: &str) -> Result<u64, SQLError>;
    fn try_query_table(
        &self,
        table: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String>;
    fn try_query_column_stats(&self, table: &str) -> Result<BTreeMap<String, ColumnStats>, String>;
    /// Read all four graph populations under one snapshot before returning.
    fn graph_snapshot(&self, graph: &str) -> Result<Option<GraphStatisticsSnapshot>, String>;
}
