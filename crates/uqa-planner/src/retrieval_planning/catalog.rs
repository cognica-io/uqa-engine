//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only catalog and retained index interfaces for retrieval costing.

use crate::ColumnStatistics;
use std::collections::BTreeMap;
use uqa_core::{catalog_index::CatalogIndexSnapshot, Edge, Predicate, Vertex};
use uqa_sql::SQLError;

/// The implementation retains the actual text-index read guard through all analyzer and frequency reads. Resource and storage failures retain their typed SQL diagnostics.
pub trait TextStatisticsRead {
    fn analyze(&self, field: &str, query: &str) -> Result<Vec<String>, SQLError>;
    fn doc_freq(&self, field: &str, term: &str) -> Result<u64, SQLError>;
    fn doc_freq_any_field(&self, term: &str) -> Result<u64, SQLError>;
    /// Observe one query while the native adapter retains analyzer output and provider workspace under its captured request allowance. Ordinary catalog adapters preserve their existing projection methods.
    fn query_doc_freq_with_control(
        &self,
        field: Option<&str>,
        query: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<u64, SQLError> {
        control.check()?;
        let Some(field) = field else {
            return self.query_doc_freq_any_field(query);
        };
        let terms = self.analyze_utf16(field, query)?;
        let mut frequency = 0_u64;
        for term in terms {
            control.check()?;
            frequency = frequency.saturating_add(self.doc_freq_utf16(field, &term)?);
        }
        control.check()?;
        Ok(frequency)
    }
    /// Field inventory for analyzed all-field queries. Legacy scalar adapters may omit it.
    fn field_names(&self) -> Result<Option<Vec<String>>, SQLError> {
        Ok(None)
    }
    /// Estimate all-field query support using each field's retained search revision.
    fn query_doc_freq_any_field(&self, query: &str) -> Result<u64, SQLError> {
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

    fn analyze_utf16(&self, field: &str, query: &str) -> Result<Vec<Vec<u16>>, SQLError> {
        Ok(self
            .analyze(field, query)?
            .into_iter()
            .map(|term| term.encode_utf16().collect())
            .collect())
    }
    fn doc_freq_utf16(&self, field: &str, term: &[u16]) -> Result<u64, SQLError> {
        self.doc_freq(
            field,
            &String::from_utf16(term).map_err(|error| {
                SQLError::Internal(format!("execute read document frequency: {error}"))
            })?,
        )
    }
}
/// The implementation retains the actual vector-index registry read guard.
pub trait VectorStatisticsRead {
    fn dimensions(&self, field: &str) -> Option<u32>;
    /// Query-independent facts for a retained field, including plans whose arguments are evaluated later.
    fn diskann_index_statistics(
        &self,
        _field: &str,
    ) -> Result<Option<uqa_core::DiskANNIndexStats>, SQLError> {
        Ok(None)
    }
    /// Physical facts and numeric route from this retained field view. No search or logical vector observation is performed.
    fn diskann_query_statistics(
        &self,
        _field: &str,
        _query: &[f32],
    ) -> Result<Option<uqa_core::DiskANNQueryStats>, SQLError> {
        Ok(None)
    }
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
    /// Retain the invoking allowance for statistics production. Standalone catalogs may use ordinary allocation.
    fn production_resources(&self) -> Result<Option<crate::PlanningResources>, SQLError> {
        Ok(None)
    }

    fn has_table(&self, table: &str) -> Result<bool, String>;
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String>;
    fn list_catalog_indexes(&self) -> Result<CatalogIndexSnapshot, String>;
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
    fn try_query_column_stats(&self, table: &str) -> Result<ColumnStatistics, String>;
    /// Read all four graph populations under one snapshot before returning.
    fn graph_snapshot(&self, graph: &str) -> Result<Option<GraphStatisticsSnapshot>, String>;
}
