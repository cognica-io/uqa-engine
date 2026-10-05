//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{catalog::resolution::RelationResolution, plan::UnifiedPlan};
use std::cell::RefCell;

#[derive(Default)]
struct Catalog {
    bound: RefCell<Vec<String>>,
    visible: RefCell<Vec<String>>,
}

impl StoredRelationCatalog for Catalog {
    fn resolve_age_label_relation_name(&self, _: &str) -> Result<Option<String>, SQLError> {
        Ok(None)
    }

    fn resolve_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        self.visible.borrow_mut().push(reference.into());
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "permission denied for schema hidden".into(),
        })
    }

    fn resolve_loaded_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(reference)
    }

    fn resolve_bound_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.bound.borrow_mut().push(reference.into());
        Ok(match reference {
            "hidden.remote" => RelationResolution::Found(reference.into(), "foreign table"),
            "hidden.visible" => RelationResolution::Found(reference.into(), "view"),
            "hidden.counter" => RelationResolution::Found(reference.into(), "sequence"),
            _ => RelationResolution::MissingRelation,
        })
    }
}

impl StoredQuerySequences for Catalog {
    fn query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("query has no sequence reference")
    }

    fn loaded_query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("query has no sequence reference")
    }
}

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query")
    };
    *plan
}

fn bind(
    catalog: &Catalog,
    query: &mut QueryPlan,
    mode: RelationLookupMode,
    loaded: bool,
) -> Result<bool, SQLError> {
    bind_stored_query_relations(
        &StoredQueryBindingContext {
            relations: catalog,
            lookup_mode: mode,
            sequences: catalog,
            temporary_schema: "pg_temp_1",
            transition_relations: &BTreeSet::new(),
        },
        query,
        "SQL routine body",
        false,
        loaded,
    )
}

#[test]
fn restored_queries_bind_private_sources_without_caller_namespace_authority() {
    let catalog = Catalog::default();
    let mut plan = query(
        "WITH local_rows AS (SELECT * FROM hidden.remote) SELECT * FROM local_rows JOIN (SELECT * FROM hidden.visible) v ON true",
    );
    assert!(!bind(&catalog, &mut plan, RelationLookupMode::Bound, true).unwrap());
    assert!(plan.relations_bound);
    assert_eq!(*catalog.bound.borrow(), ["hidden.remote", "hidden.visible"]);
    assert!(catalog.visible.borrow().is_empty());
}

#[test]
fn new_query_definitions_still_check_caller_namespace_authority() {
    for loaded in [false, true] {
        let catalog = Catalog::default();
        let mut plan = query("SELECT * FROM hidden.remote");
        let error = bind(&catalog, &mut plan, RelationLookupMode::Dynamic, loaded).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(error.to_string(), "permission denied for schema hidden");
        assert_eq!(*catalog.visible.borrow(), ["hidden.remote"]);
        assert!(catalog.bound.borrow().is_empty());
    }
}

#[test]
fn restored_queries_still_reject_missing_and_non_row_sources() {
    for (relation, state) in [("hidden.missing", "42P01"), ("hidden.counter", "42809")] {
        let catalog = Catalog::default();
        let mut plan = query(&format!("SELECT * FROM {relation}"));
        let error = bind(&catalog, &mut plan, RelationLookupMode::Bound, true).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(*catalog.bound.borrow(), [relation]);
        assert!(catalog.visible.borrow().is_empty());
    }
}
