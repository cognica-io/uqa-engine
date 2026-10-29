//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::retrieval_planning::{
    GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable,
    TextStatisticsRead, VectorStatisticsRead,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use uqa_core::{
    catalog_index::CatalogIndexSnapshot, CancellationToken, DiskANNQueryStats, Predicate,
};
use uqa_core::{
    DiskANNIndexStats, VectorGeneration, VectorPopulationStats, VectorQueryRoute, VectorReadStats,
};
use uqa_sql::SQLError;

#[derive(Default)]
pub(super) struct Inputs {
    pub events: RefCell<Vec<&'static str>>,
    pub text: RefCell<Vec<(usize, usize)>>,
    pub vectors: RefCell<Vec<(usize, usize)>>,
    pub error: RefCell<Option<SQLError>>,
    pub cancel: Option<CancellationToken>,
    pub unwind_vector: bool,
    pub dimensions: u32,
    pub physical: Option<DiskANNQueryStats>,
    pub analyses: Cell<usize>,
    pub frequencies: Cell<usize>,
}

impl Inputs {
    pub fn new() -> Self {
        Self {
            events: RefCell::new(Vec::with_capacity(64)),
            text: RefCell::new(Vec::with_capacity(16)),
            vectors: RefCell::new(Vec::with_capacity(16)),
            dimensions: 2,
            ..Self::default()
        }
    }
}

pub(super) struct Catalog(pub Rc<Inputs>);
struct Table(Rc<Inputs>);
struct Text<'a>(&'a Inputs);
struct Vectors<'a>(&'a Inputs);

impl RetrievalPlanningCatalog for Catalog {
    fn has_table(&self, _: &str) -> Result<bool, String> {
        unreachable!()
    }
    fn resolve_table_name(&self, _: &str) -> Result<Option<String>, String> {
        unreachable!()
    }
    fn list_catalog_indexes(&self) -> Result<CatalogIndexSnapshot, String> {
        unreachable!()
    }
    fn value_index_cardinality(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        unreachable!()
    }
    fn value_index_supports(&self, _: &str, _: &str, _: &Predicate) -> Result<bool, String> {
        unreachable!()
    }
    fn text_top_k_capabilities(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<crate::TextTopKCapabilities, SQLError> {
        unreachable!()
    }
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        Ok(7)
    }
    fn try_query_table(
        &self,
        _: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        Ok(Some(Box::new(Table(Rc::clone(&self.0)))))
    }
    fn try_query_column_stats(&self, _: &str) -> Result<crate::ColumnStatistics, String> {
        unreachable!()
    }
    fn graph_snapshot(&self, _: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        unreachable!()
    }
}

impl RetrievalStatisticsTable for Table {
    fn text_index(&self) -> Box<dyn TextStatisticsRead + '_> {
        self.0.events.borrow_mut().push("text open");
        Box::new(Text(&self.0))
    }
    fn vector_indexes(&self) -> Box<dyn VectorStatisticsRead + '_> {
        self.0.events.borrow_mut().push("vectors open");
        Box::new(Vectors(&self.0))
    }
}

impl TextStatisticsRead for Text<'_> {
    fn analyze(&self, _: &str, _: &str) -> Result<Vec<String>, SQLError> {
        unreachable!("lossless analyzer is implemented")
    }
    fn analyze_utf16(&self, field: &str, query: &str) -> Result<Vec<Vec<u16>>, SQLError> {
        self.0.events.borrow_mut().push("analyze");
        self.0
            .text
            .borrow_mut()
            .push((field.as_ptr() as usize, query.as_ptr() as usize));
        self.0.analyses.set(self.0.analyses.get() + 1);
        if self.0.analyses.get() == 2 {
            if let Some(cancel) = &self.0.cancel {
                cancel.cancel();
            }
            if let Some(error) = self.0.error.borrow_mut().take() {
                return Err(error);
            }
        }
        Ok(vec![vec![1], vec![2]])
    }
    fn doc_freq(&self, _: &str, _: &str) -> Result<u64, SQLError> {
        unreachable!("lossless lookup is implemented")
    }
    fn doc_freq_utf16(&self, _: &str, _: &[u16]) -> Result<u64, SQLError> {
        self.0.events.borrow_mut().push("frequency");
        self.0.frequencies.set(self.0.frequencies.get() + 1);
        Ok(u64::MAX)
    }
    fn doc_freq_any_field(&self, query: &str) -> Result<u64, SQLError> {
        self.0.events.borrow_mut().push("all fields");
        self.0.text.borrow_mut().push((0, query.as_ptr() as usize));
        Ok(u64::MAX)
    }
}

impl VectorStatisticsRead for Vectors<'_> {
    fn dimensions(&self, field: &str) -> Option<u32> {
        self.0.events.borrow_mut().push("dimensions");
        (field != "missing").then_some(self.0.dimensions)
    }
    fn diskann_query_statistics(
        &self,
        field: &str,
        query: &[f32],
    ) -> Result<Option<DiskANNQueryStats>, SQLError> {
        self.0.events.borrow_mut().push("physical");
        self.0
            .vectors
            .borrow_mut()
            .push((field.as_ptr() as usize, query.as_ptr() as usize));
        if self.0.unwind_vector {
            std::panic::resume_unwind(Box::new("vector callback"));
        }
        Ok(self.0.physical.clone())
    }
}

impl Drop for Text<'_> {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("text close");
    }
}
impl Drop for Vectors<'_> {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("vectors close");
    }
}
impl Drop for Table {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("table close");
    }
}

pub(super) fn physical() -> DiskANNQueryStats {
    DiskANNQueryStats {
        index: DiskANNIndexStats {
            generation: VectorGeneration {
                database: [1; 16],
                table: 2,
                index: 3,
                generation: 4,
            },
            dimensions: 2,
            max_degree: 8,
            search_list_size: 12,
            beam_width: 2,
            pq_bytes: 4,
            pq_centroids: Some(16),
            node_slot_bytes: 64,
            node_fragments: 1,
            graph_pages: 10,
            graph_edges: Some(30),
            page_bytes: 4096,
            populations: VectorPopulationStats {
                base_documents: Some(10),
                base_vectors: 10,
                graph_nodes: 10,
                side_vectors: 10,
                current_vectors: Some(10),
                changed_vectors: Some(0),
            },
            reads: VectorReadStats {
                resident_bytes: 16,
                cache_bytes: 4096,
                max_record_bytes: 4096,
                max_in_flight_page_bytes: 4096,
                max_batch_pages: 1,
                read_concurrency: 1,
            },
        },
        query_route: VectorQueryRoute::Approximate,
    }
}
