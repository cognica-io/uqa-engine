//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn relation_schema(name: &str) -> Vec<(String, ColumnType)> {
    resolve_virtual_relation(&[], name).unwrap().schema()
}

#[test]
fn pg18_catalog_shapes_include_empty_relations_and_removed_columns() {
    let description = relation_schema("pg_catalog.pg_description");
    assert_eq!(description.len(), 4);
    assert_eq!(description[0], ("objoid".into(), ColumnType::Oid));

    let attrdef = relation_schema("pg_attrdef");
    assert_eq!(
        attrdef
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["oid", "adrelid", "adnum", "adbin"]
    );
}

#[test]
fn pg18_information_schema_domains_retain_their_oid_identity() {
    let routines = relation_schema("information_schema.routines");
    assert_eq!(routines.len(), 82);
    assert!(matches!(
        routines[0].1,
        ColumnType::Domain { oid: 13_312, .. }
    ));
    assert!(matches!(
        routines[54].1,
        ColumnType::Domain { oid: 13_318, .. }
    ));
}

#[test]
fn ag_catalog_relations_resolve_qualified_or_through_the_search_path() {
    let resolution = Vec::new();
    assert_eq!(
        resolve_virtual_relation(&resolution, "ag_catalog.ag_graph"),
        Some(VirtualRelation::AgGraph)
    );
    assert_eq!(
        resolve_virtual_relation(&resolution, "AG_CATALOG.AG_LABEL"),
        Some(VirtualRelation::AgLabel)
    );
    assert_eq!(resolve_virtual_relation(&resolution, "ag_graph"), None);
    let resolution = vec!["ag_catalog".into(), "$user".into(), "public".into()];
    assert_eq!(
        resolve_virtual_relation(&resolution, "ag_graph"),
        Some(VirtualRelation::AgGraph)
    );
    assert_eq!(
        resolve_virtual_relation(&resolution, "ag_label"),
        Some(VirtualRelation::AgLabel)
    );
    assert_eq!(
        resolve_virtual_relation(&resolution, "public.ag_graph"),
        None
    );

    let graph = relation_schema("ag_catalog.ag_graph");
    assert_eq!(
        graph
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["graphid", "name", "namespace"]
    );
    assert_eq!(graph[2].1, ColumnType::Regnamespace);
    let label = relation_schema("ag_catalog.ag_label");
    assert_eq!(
        label
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["name", "graph", "id", "kind", "relation", "seq_name"]
    );
    assert!(matches!(&label[2].1, ColumnType::Domain { name, .. } if name == "label_id"));
    assert!(matches!(&label[3].1, ColumnType::Domain { name, .. } if name == "label_kind"));
}
