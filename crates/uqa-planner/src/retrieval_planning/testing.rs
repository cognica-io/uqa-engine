//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable, SQLError,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use uqa_core::{catalog_index::CatalogIndexRow, Predicate};

pub(super) struct Inputs {
    pub(super) support: Result<bool, &'static str>,
    pub(super) probes: RefCell<Vec<String>>,
    pub(super) text_reads: RefCell<Vec<(String, String, String)>>,
}

impl Inputs {
    pub(super) fn new(support: Result<bool, &'static str>) -> Self {
        Self {
            support,
            probes: RefCell::new(Vec::new()),
            text_reads: RefCell::new(Vec::new()),
        }
    }
}

impl RetrievalPlanningCatalog for Inputs {
    fn has_table(&self, _: &str) -> Result<bool, String> {
        panic!("access selection follows optimizer catalog binding")
    }
    fn resolve_table_name(&self, _: &str) -> Result<Option<String>, String> {
        panic!("access selection follows optimizer catalog binding")
    }
    fn list_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, String> {
        panic!("access selection follows optimizer catalog binding")
    }
    fn value_index_cardinality(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        panic!("access selection follows optimizer costing")
    }
    fn value_index_supports(
        &self,
        table: &str,
        field: &str,
        _: &Predicate,
    ) -> Result<bool, String> {
        assert_eq!(table, "docs");
        self.probes.borrow_mut().push(field.into());
        self.support.map_err(str::to_owned)
    }
    fn text_top_k_capabilities(
        &self,
        table: &str,
        field: &str,
        query: &str,
    ) -> Result<crate::TextTopKCapabilities, SQLError> {
        self.text_reads
            .borrow_mut()
            .push((table.into(), field.into(), query.into()));
        Ok(crate::TextTopKCapabilities {
            analyzed_term_count: 2,
            indexed_document_count: 100,
        })
    }
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        panic!("access selection follows optimizer statistics binding")
    }
    fn try_query_table(
        &self,
        _: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        panic!("access selection follows optimizer statistics binding")
    }
    fn try_query_column_stats(
        &self,
        _: &str,
    ) -> Result<BTreeMap<String, crate::ColumnStats>, String> {
        panic!("access selection follows optimizer statistics binding")
    }
    fn graph_snapshot(&self, _: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        panic!("access selection follows optimizer statistics binding")
    }
}
