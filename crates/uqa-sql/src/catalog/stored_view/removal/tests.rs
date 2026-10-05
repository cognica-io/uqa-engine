//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    binding::view_dependencies::{bind_query_plan_relations, query_plan_references_relation},
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
        security: crate::catalog::security::BoundTableSecurity::owner(
            crate::catalog::roles::RoleIdentity {
                oid: 42,
                object_id: [42; 16],
            },
        ),
        definition: crate::catalog::stored_view::StoredViewDefinition {
            object_id: [7; 16],
            catalog_oids: None,
            row_type_array_name: None,
            query: *query,
            output_columns: Some(vec!["public_value".into()]),
            persistence,
            options: Vec::new(),
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
        },
    }
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
