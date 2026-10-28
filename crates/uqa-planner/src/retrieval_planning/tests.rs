//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnStatistics;
use std::{cell::RefCell, rc::Rc};
use uqa_core::{
    catalog_index::{CatalogIndexRow, CatalogIndexSnapshot},
    Predicate,
};

#[derive(Clone, Default)]
struct Catalog {
    events: Rc<RefCell<Vec<String>>>,
}
struct Table(Catalog);
struct Text<'a>(&'a Catalog);

impl RetrievalPlanningCatalog for Catalog {
    fn has_table(&self, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String> {
        Ok(Some(table.into()))
    }
    fn list_catalog_indexes(&self) -> Result<CatalogIndexSnapshot, String> {
        Ok(CatalogIndexSnapshot::default())
    }
    fn value_index_cardinality(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        panic!("no filter candidates")
    }
    fn value_index_supports(&self, _: &str, _: &str, _: &Predicate) -> Result<bool, String> {
        panic!("this failure must precede access selection")
    }
    fn text_top_k_capabilities(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<crate::TextTopKCapabilities, SQLError> {
        panic!("these trees do not plan physical text limits")
    }
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        Ok(10)
    }
    fn try_query_table(
        &self,
        _: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        Ok(Some(Box::new(Table(self.clone()))))
    }
    fn try_query_column_stats(&self, _: &str) -> Result<ColumnStatistics, String> {
        panic!("must stop at the text index error")
    }
    fn graph_snapshot(&self, _: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        panic!("must stop at the text index error")
    }
}
impl RetrievalStatisticsTable for Table {
    fn text_index(&self) -> Box<dyn TextStatisticsRead + '_> {
        self.0.events.borrow_mut().push("read text".into());
        Box::new(Text(&self.0))
    }
    fn vector_indexes(&self) -> Box<dyn VectorStatisticsRead + '_> {
        panic!("must stop at the text index error")
    }
}
impl TextStatisticsRead for Text<'_> {
    fn analyze(&self, _: &str, query: &str) -> Result<Vec<String>, String> {
        self.0.events.borrow_mut().push("analyze".into());
        Ok(query.split_whitespace().map(str::to_owned).collect())
    }
    fn doc_freq(&self, _: &str, term: &str) -> Result<u64, String> {
        self.0.events.borrow_mut().push(format!("frequency:{term}"));
        if term == "failing" {
            Err("unavailable".into())
        } else {
            Ok(1)
        }
    }
    fn doc_freq_any_field(&self, _: &str) -> Result<u64, String> {
        panic!("field is explicit")
    }
}
impl Drop for Text<'_> {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("release text".into());
    }
}
impl Drop for Table {
    fn drop(&mut self) {
        self.0.events.borrow_mut().push("release table".into());
    }
}

#[test]
fn first_frequency_error_releases_readers_before_stopping_catalog_work() {
    let catalog = Catalog::default();
    let tree = OperatorTree::Term {
        query: "first failing later".into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    };
    assert!(
        matches!(query_optimizer(&catalog, "docs", &tree), Err(SQLError::Internal(message)) if message == "execute read document frequency: unavailable")
    );
    assert_eq!(
        *catalog.events.borrow(),
        [
            "read text",
            "analyze",
            "frequency:first",
            "frequency:failing",
            "release text",
            "release table"
        ]
    );
}

struct IndexCatalog {
    events: RefCell<Vec<&'static str>>,
    rows: CatalogIndexSnapshot,
}

impl RetrievalPlanningCatalog for IndexCatalog {
    fn has_table(&self, table: &str) -> Result<bool, String> {
        assert_eq!(table, "docs");
        self.events.borrow_mut().push("has table");
        Ok(true)
    }
    fn resolve_table_name(&self, table: &str) -> Result<Option<String>, String> {
        assert_eq!(table, "docs");
        self.events.borrow_mut().push("resolve table");
        Ok(Some("public.docs".into()))
    }
    fn list_catalog_indexes(&self) -> Result<CatalogIndexSnapshot, String> {
        self.events.borrow_mut().push("list indexes");
        Ok(self.rows.clone())
    }
    fn value_index_cardinality(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        assert_eq!(table, "docs");
        assert_eq!(field, "id");
        assert_eq!(predicate, &Predicate::Equals(uqa_core::Value::Int(1)));
        self.events.borrow_mut().push("cardinality");
        Ok(Some(7))
    }
    fn value_index_supports(&self, _: &str, _: &str, _: &Predicate) -> Result<bool, String> {
        panic!("this failure must precede access selection")
    }
    fn text_top_k_capabilities(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<crate::TextTopKCapabilities, SQLError> {
        panic!("these trees do not plan physical text limits")
    }
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        panic!("invalid catalog metadata must precede statistics reads")
    }
    fn try_query_table(
        &self,
        _: &str,
    ) -> Result<Option<Box<dyn RetrievalStatisticsTable>>, String> {
        panic!("invalid catalog metadata must precede table retention")
    }
    fn try_query_column_stats(&self, _: &str) -> Result<ColumnStatistics, String> {
        panic!("invalid catalog metadata must precede column statistics")
    }
    fn graph_snapshot(&self, _: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        panic!("invalid catalog metadata must precede graph snapshots")
    }
}

#[test]
fn invalid_index_metadata_stops_planning_before_reading_runtime_statistics() {
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::new()),
        rows: vec![CatalogIndexRow {
            relation: uqa_core::RelationIdentity::new("public", "broken_index"),
            index_type: "btree".into(),
            table_name: "public.docs".into(),
            columns_json: "[".into(),
            parameters_json: "{}".into(),
            definition_json: None,
        }]
        .into(),
    };
    let tree = OperatorTree::Term {
        query: "rust".into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    };
    assert!(
        matches!(query_optimizer(&catalog, "docs", &tree), Err(SQLError::Internal(message))
        if message.starts_with("decode catalog index `public.broken_index` columns:"))
    );
    assert_eq!(
        *catalog.events.borrow(),
        ["has table", "resolve table", "list indexes"]
    );
}

#[test]
fn shared_index_rows_preserve_candidate_order_without_copying_unrelated_payloads() {
    let row = |name: &str| CatalogIndexRow {
        relation: uqa_core::RelationIdentity::new("public", name),
        index_type: "BTREE".into(),
        table_name: "public.docs".into(),
        columns_json: "[\"id\"]".into(),
        parameters_json: "{}".into(),
        definition_json: None,
    };
    let mut unrelated = row("ignored");
    unrelated.table_name = "public.other".into();
    unrelated.columns_json = "x".repeat(4 * 1024 * 1024);
    let catalog = IndexCatalog {
        events: RefCell::new(Vec::with_capacity(4)),
        rows: vec![unrelated, row("z_first"), row("a_second")].into(),
    };
    let tree = OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::Equals(uqa_core::Value::Int(1)),
        source: None,
    };
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(index_candidates(&catalog, "docs", &tree).unwrap());
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    let output = output.unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].index_name, "public.z_first");
    assert_eq!(output[0].table_name, "docs");
    assert_eq!(output[0].field, "id");
    assert_eq!(
        output[0].predicate,
        Predicate::Equals(uqa_core::Value::Int(1))
    );
    assert_eq!(
        output[0].scan_cost.to_bits(),
        crate::CostEstimator::default()
            .estimate_unary(crate::OperatorKind::IndexScan, 7.0)
            .total()
            .to_bits()
    );
    assert_eq!(
        *catalog.events.borrow(),
        ["has table", "resolve table", "list indexes", "cardinality"]
    );
}
