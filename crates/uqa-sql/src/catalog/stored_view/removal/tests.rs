//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    binding::view_dependencies::bind_query_plan_relations,
    catalog::{stored_view::references::rewritten_relation_references, view::StoredViewKind},
    plan::UnifiedPlan,
};
use std::collections::BTreeSet;

fn view(sql: &str, persistence: RelationPersistence) -> StoredView {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    bind_query_plan_relations(&mut query, &BTreeSet::new(), &mut |name| {
        Ok::<_, String>(name.to_string())
    })
    .unwrap();
    StoredView {
        object_id: [7; 16],
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        query: *query,
        output_columns: Some(vec!["public_value".into()]),
        persistence,
        options: Vec::new(),
        kind: StoredViewKind::View,
        materialized_rows: Vec::new(),
        materialized_column_types: Vec::new(),
        populated: true,
    }
}

#[test]
fn temporary_view_layers_separate_transitive_dependencies_from_unrelated_views() {
    let inner = RelationIdentity::new("pg_temp_7", "inner_view");
    let outer = RelationIdentity::new("pg_temp_7", "outer_view");
    let views = BTreeMap::from([
        (
            inner.clone(),
            view(
                "SELECT value FROM pg_temp_7.base",
                RelationPersistence::Temporary,
            ),
        ),
        (
            outer.clone(),
            view(
                "SELECT value FROM pg_temp_7.inner_view",
                RelationPersistence::Temporary,
            ),
        ),
        (
            RelationIdentity::new("public", "unrelated"),
            view(
                "SELECT value FROM public.base",
                RelationPersistence::Permanent,
            ),
        ),
    ]);
    let layers = temporary_view_dependency_layers(
        "pg_temp_7.base",
        RelationIdentity::new("pg_temp_7", "base"),
        &views,
    )
    .unwrap();
    assert_eq!(layers, vec![vec![inner], vec![outer]]);
}

#[test]
fn temporary_view_preflight_rejects_a_permanent_transitive_dependent() {
    let views = BTreeMap::from([
        (
            RelationIdentity::new("pg_temp_7", "inner_view"),
            view(
                "SELECT value FROM pg_temp_7.base",
                RelationPersistence::Temporary,
            ),
        ),
        (
            RelationIdentity::new("public", "invalid_view"),
            view(
                "SELECT value FROM pg_temp_7.inner_view",
                RelationPersistence::Permanent,
            ),
        ),
    ]);
    let error = temporary_view_dependency_layers(
        "pg_temp_7.base",
        RelationIdentity::new("pg_temp_7", "base"),
        &views,
    )
    .unwrap_err();
    assert!(error.contains("non-temporary dependent view `public.invalid_view`"));
    assert_eq!(views.len(), 2);
}

#[test]
fn relation_rewrite_preserves_view_metadata_and_leaves_the_original_registry_untouched() {
    let original = view(
        "SELECT b.value FROM public.base AS b",
        RelationPersistence::Permanent,
    );
    let before = serde_json::to_value(&original).unwrap();
    let security = original.security();
    let views = BTreeMap::from([
        (RelationIdentity::new("public", "dependent"), original),
        (
            RelationIdentity::new("public", "unrelated"),
            view(
                "SELECT value FROM other.base",
                RelationPersistence::Permanent,
            ),
        ),
    ]);
    let source = RelationIdentity::new("public", "base");
    let target = RelationIdentity::new("renamed", "base");
    let updates =
        rewritten_relation_references(&views, &BTreeMap::from([(source.clone(), target.clone())]))
            .unwrap();
    assert_eq!(updates.len(), 1);
    let (name, candidate) = &updates[0];
    assert_eq!(name, &RelationIdentity::new("public", "dependent"));
    assert!(query_plan_references_relation(
        &candidate.query,
        &target,
        &BTreeSet::new()
    ));
    assert!(!query_plan_references_relation(
        &candidate.query,
        &source,
        &BTreeSet::new()
    ));
    assert_eq!(candidate.security(), security);
    assert_eq!(candidate.object_id, [7; 16]);
    assert_eq!(candidate.output_columns, Some(vec!["public_value".into()]));
    assert_eq!(serde_json::to_value(&views[name]).unwrap(), before);
}

#[test]
fn rule_dependency_error_retains_target_and_dependent_order() {
    let error = ensure_no_rule_dependents(
        &["public.first".into(), "public.second".into()],
        vec![
            (RelationIdentity::new("app", "owner_b"), "rule_b".into()),
            (RelationIdentity::new("app", "owner_a"), "rule_a".into()),
        ],
    )
    .unwrap_err();
    let SQLError::Routine { sqlstate, message } = error else {
        panic!("dependency error");
    };
    assert_eq!(sqlstate, "2BP01");
    assert_eq!(message, "cannot drop view public.first, public.second because other objects depend on it: rule rule_b on table app.owner_b, rule rule_a on table app.owner_a");
}
