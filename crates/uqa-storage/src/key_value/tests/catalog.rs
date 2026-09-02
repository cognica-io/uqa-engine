//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn key_value_catalog_preserves_core_registries() {
    let catalog = KeyValueCatalog::new(store());
    let table_acl = vec![crate::catalog::TableAclEntry {
        role: "docs_reader".into(),
        grantor: Some("docs_owner".into()),
        privileges: crate::catalog::TablePrivileges {
            select: true,
            ..crate::catalog::TablePrivileges::default()
        },
        grant_options: crate::catalog::TablePrivileges::default(),
    }];
    catalog.set_metadata("schema_version", "10").unwrap();
    assert_eq!(
        catalog.get_metadata("schema_version").unwrap().as_deref(),
        Some("10")
    );
    catalog.save_schema("public").unwrap();
    catalog.save_schema("empty_app").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "docs"),
            role_owner: "docs_owner".into(),
            acl: Some(table_acl.clone()),
            column_acls: std::collections::BTreeMap::default(),
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec!["title".into()],
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    catalog.save_model("reranker", "{\"model\":1}").unwrap();
    catalog
        .save_scoring_params("bm25", "{\"alpha\":0.5}")
        .unwrap();
    catalog.save_named_graph("g").unwrap();
    catalog.save_vertex(1, "Person", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();

    assert_eq!(
        catalog.load_tables().unwrap()[0].relation.qualified_name(),
        "public.docs"
    );
    assert_eq!(catalog.load_tables().unwrap()[0].role_owner, "docs_owner");
    assert_eq!(catalog.load_tables().unwrap()[0].acl, Some(table_acl));
    assert_eq!(
        catalog.load_model("reranker").unwrap().as_deref(),
        Some("{\"model\":1}")
    );
    assert_eq!(catalog.load_named_graphs().unwrap(), vec!["g"]);
    assert_eq!(
        catalog.load_schemas().unwrap(),
        vec!["empty_app".to_string(), "public".to_string()]
    );
    assert_eq!(catalog.load_vertices().unwrap()[0].0, 1);
    assert_eq!(
        catalog.load_graph_memberships().unwrap(),
        vec![("vertex".into(), 1, "g".into())]
    );
}

#[test]
fn key_value_catalog_rejects_a_table_without_its_parent_schema() {
    let catalog = KeyValueCatalog::new(store());
    let error = catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("missing", "docs"),
            role_owner: "docs_owner".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
            object_id: [2; 16],
            storage_generation: [3; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .expect_err("a relation write must not repair a missing parent schema");

    assert!(error
        .to_string()
        .contains("schema `missing` does not exist for relation `missing.docs`"));
    assert!(catalog.load_schemas().unwrap().is_empty());
    assert!(catalog.load_tables().unwrap().is_empty());
}
