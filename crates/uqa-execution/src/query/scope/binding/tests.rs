//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::empty_scope;
use uqa_sql::catalog::resolution::RelationLookupMode;

#[test]
fn source_metadata_requires_the_same_catalog_and_namespace() {
    let scope = empty_scope();
    let inputs = SourceBindingScope::capture(&scope).unwrap();
    assert!(inputs.matches(&scope));
    assert!(inputs.matches(&scope.clone()));
    assert!(!inputs.matches(&empty_scope()));
    let mut bound = scope.clone();
    bound
        .set_relation_lookup_mode(RelationLookupMode::Bound)
        .unwrap();
    assert!(inputs.matches(&bound));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .search_path
        .insert(0, "other".into());
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .temporary_namespace_allocated = true;
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .temporary_schema
        .push_str("_other");
    assert!(!inputs.matches(&changed));
}

#[test]
fn source_metadata_does_not_cross_cte_definitions_or_recursive_descriptors() {
    let mut scope = empty_scope();
    let uqa_sql::plan::UnifiedPlan::Query(mut query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("WITH input AS (SELECT 1 AS x) SELECT * FROM input")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    scope.insert_deferred(query.ctes.remove(0).into());
    let inputs = SourceBindingScope::capture(&scope).unwrap();
    assert!(inputs.matches(&scope.clone()));
    let mut changed = scope.clone();
    let original = changed.deferred_ctes.get("input").unwrap();
    changed
        .deferred_ctes
        .insert("input".into(), (**original).clone().into());
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed.recursive_control_widths.insert("input".into(), 1);
    assert!(!inputs.matches(&changed));
    let mut changed = scope;
    changed.non_returning_ctes.insert("input".into());
    assert!(!inputs.matches(&changed));
}
