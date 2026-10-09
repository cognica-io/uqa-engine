//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn expression_output_metadata_follows_its_retained_generation() {
    let catalog = fixture(0);
    let resolution = CatalogServices::default().resolution;
    let old = AliasConstantOutput::build(&catalog, &resolution).unwrap();
    let mut snapshot = catalog.snapshot().clone();
    let domains = Arc::make_mut(&mut snapshot.definitions.domains);
    let mut domain = domains.remove("hidden.positive").unwrap();
    domain.identity.name = "renamed".into();
    domain.definition.name = "hidden.renamed".into();
    domains.insert("hidden.renamed".into(), domain);
    let current = CatalogReadView::new(snapshot.clone());
    let new = AliasConstantOutput::build(&current, &resolution).unwrap();
    assert_eq!(
        new.text(&ColumnType::Regtype, 60_000),
        Some("hidden.renamed".into())
    );
    assert_eq!(
        old.text(&ColumnType::Regtype, 60_000),
        Some("hidden.positive".into())
    );
    assert!(!Arc::ptr_eq(&old.catalog, &new.catalog));
    snapshot.definitions.domains = Arc::default();
    let removed = CatalogReadView::new(snapshot);
    let removed = AliasConstantOutput::build(&removed, &resolution).unwrap();
    assert_eq!(removed.text(&ColumnType::Regtype, 60_000), None);
    assert!(Arc::ptr_eq(
        &old.catalog,
        &AliasConstantOutput::build(&catalog, &resolution)
            .unwrap()
            .catalog
    ));
}

#[test]
fn failed_expression_output_metadata_is_not_published() {
    let catalog = fixture(0);
    let mut snapshot = catalog.snapshot().clone();
    Arc::make_mut(
        &mut Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
            .get_mut("hidden.echo")
            .unwrap()[0],
    )
    .def
    .catalog_oid = None;
    let invalid = CatalogReadView::new(snapshot);
    let resolution = CatalogServices::default().resolution;
    let before = OUTPUT_METADATA_BUILDS.get();
    for _ in 0..2 {
        let Err(error) = AliasConstantOutput::build(&invalid, &resolution) else {
            panic!("invalid identity");
        };
        assert!(error
            .to_string()
            .contains("routine `hidden.echo` has no catalog object identity"));
    }
    assert_eq!(OUTPUT_METADATA_BUILDS.get() - before, 2);
    assert!(AliasConstantOutput::build(&catalog, &resolution).is_ok());
}
