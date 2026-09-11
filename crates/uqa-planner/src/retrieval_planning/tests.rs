//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnStats;
use std::{cell::RefCell, rc::Rc};
use uqa_core::{catalog_index::CatalogIndexRow, Predicate};

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
    fn list_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, String> {
        Ok(Vec::new())
    }
    fn value_index_cardinality(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        panic!("no filter candidates")
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
    fn try_query_column_stats(&self, _: &str) -> Result<BTreeMap<String, ColumnStats>, String> {
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

struct BrokenIndexCatalog {
    events: RefCell<Vec<&'static str>>,
}

impl RetrievalPlanningCatalog for BrokenIndexCatalog {
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
    fn list_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, String> {
        self.events.borrow_mut().push("list indexes");
        Ok(vec![CatalogIndexRow {
            relation: uqa_core::RelationIdentity::new("public", "broken_index"),
            index_type: "btree".into(),
            table_name: "public.docs".into(),
            columns_json: "[".into(),
            parameters_json: "{}".into(),
            definition_json: None,
        }])
    }
    fn value_index_cardinality(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        panic!("invalid catalog metadata must precede index access")
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
    fn try_query_column_stats(&self, _: &str) -> Result<BTreeMap<String, ColumnStats>, String> {
        panic!("invalid catalog metadata must precede column statistics")
    }
    fn graph_snapshot(&self, _: &str) -> Result<Option<GraphStatisticsSnapshot>, String> {
        panic!("invalid catalog metadata must precede graph snapshots")
    }
}

#[test]
fn invalid_index_metadata_stops_planning_before_reading_runtime_statistics() {
    let catalog = BrokenIndexCatalog {
        events: RefCell::new(Vec::new()),
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
