//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::projection::{build_requested_catalog_rows, CatalogRequest};
use uqa_sql::plan::source_projection::SourceProjection;

fn generated_fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = fixture(unrelated).snapshot().clone();
    for table in snapshot.tables.values_mut() {
        let mut columns = table.columns.to_vec();
        let mut generated = ColumnDef::nullable("generated_ref", ColumnType::Regtype);
        generated.generated = Some(uqa_sql::ast::GeneratedColumn {
            kind: uqa_sql::ast::GeneratedColumnKind::Stored,
            expression: Box::new(constant(60_000, "regtype")),
            function_dependencies: Vec::new(),
        });
        columns.push(generated);
        table.columns = columns.into();
    }
    CatalogReadView::new(snapshot)
}

fn project(
    catalog: &CatalogReadView,
    name: &str,
    request: &CatalogRequest,
) -> Vec<uqa_sql::ResultRow> {
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    build_requested_catalog_rows(
        &services.context(catalog, &output),
        catalog,
        &services.resolution,
        &services,
        name,
        request,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn narrow_catalog_sources_do_not_construct_unused_expression_metadata() {
    for unrelated in [0, 1, 128] {
        let catalog = generated_fixture(unrelated);
        let services = CatalogServices::default();
        let columns = crate::catalog::schema::virtual_relation_schema(
            &catalog,
            &services.resolution,
            "information_schema.columns",
        )
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
        let before = OUTPUT_METADATA_BUILDS.get();
        for qualifier in ["left_columns", "right_columns", "left_columns"] {
            let mut wanted = SourceProjection::default();
            wanted.extend([
                "table_name".into(),
                "column_name".into(),
                "data_type".into(),
            ]);
            let prune = BTreeMap::from([(qualifier.into(), wanted)]);
            let request = crate::query::source_projection::catalog_request(
                &columns,
                &[],
                qualifier,
                Some(&prune),
            );
            let rows = project(&catalog, "information_schema.columns", &request);
            assert_eq!(rows.len(), 3 * (unrelated + 1));
            assert!(rows.iter().all(|row| row["column_default"] == Value::Null
                && row["generation_expression"] == Value::Null));
        }
        for (name, fields) in [
            ("pg_catalog.pg_type", vec!["oid".into(), "typname".into()]),
            ("pg_catalog.pg_proc", vec!["proname".into()]),
        ] {
            assert!(!project(&catalog, name, &CatalogRequest::columns(fields)).is_empty());
        }
        assert_eq!(OUTPUT_METADATA_BUILDS.get() - before, 0);
    }
}

#[test]
fn selected_catalog_defaults_and_generated_expressions_keep_their_text() {
    for field in ["column_default", "generation_expression"] {
        let catalog = generated_fixture(0);
        let before = OUTPUT_METADATA_BUILDS.get();
        let rows = project(
            &catalog,
            "information_schema.columns",
            &CatalogRequest::columns([field.into()]),
        );
        let (column, other) = if field == "column_default" {
            ("type_ref", "generation_expression")
        } else {
            ("generated_ref", "column_default")
        };
        let selected = rows
            .iter()
            .find(|row| row["column_name"] == Value::Str(column.into()))
            .unwrap();
        assert_eq!(
            selected[field],
            Value::Str("'hidden.positive'::regtype".into())
        );
        assert!(rows.iter().all(|row| row[other] == Value::Null));
        assert_eq!(OUTPUT_METADATA_BUILDS.get() - before, 1);
    }
    for (name, field, identity, wanted) in [
        ("pg_catalog.pg_type", "typdefault", "typname", "positive"),
        ("pg_catalog.pg_type", "typdefaultbin", "typname", "positive"),
        ("pg_catalog.pg_proc", "proargdefaults", "proname", "echo"),
    ] {
        let catalog = fixture(0);
        let request = CatalogRequest::columns([field.into()]);
        let rows = project(&catalog, name, &request);
        let full = project(&catalog, name, &CatalogRequest::default());
        let selected = rows
            .iter()
            .find(|row| row[identity] == Value::Str(wanted.into()))
            .unwrap();
        let expected = full
            .iter()
            .find(|row| row[identity] == Value::Str(wanted.into()))
            .unwrap();
        assert_ne!(selected[field], Value::Null);
        assert_eq!(selected[field], expected[field]);
    }
}

#[test]
fn catalog_field_requests_follow_source_aliases_and_whole_row_demand() {
    let columns = vec![
        "column_name".into(),
        "column_default".into(),
        "generation_expression".into(),
    ];
    let aliases = vec![
        "name".into(),
        "default_value".into(),
        "generated_value".into(),
    ];
    let mut selected = SourceProjection::default();
    selected.insert("default_value".into());
    let prune = BTreeMap::from([("c".into(), selected)]);
    let request =
        crate::query::source_projection::catalog_request(&columns, &aliases, "c", Some(&prune));
    assert!(request.includes("column_default"));
    assert!(!request.includes("generation_expression"));
    for qualifier in ["", "absent"] {
        let request = crate::query::source_projection::catalog_request(
            &columns,
            &aliases,
            qualifier,
            Some(&prune),
        );
        assert!(columns.iter().all(|column| request.includes(column)));
    }
    let prune = BTreeMap::from([("c".into(), SourceProjection::retaining_all())]);
    let request =
        crate::query::source_projection::catalog_request(&columns, &aliases, "c", Some(&prune));
    assert!(columns.iter().all(|column| request.includes(column)));
    let prune = BTreeMap::from([("c".into(), SourceProjection::default())]);
    let request =
        crate::query::source_projection::catalog_request(&columns, &aliases, "c", Some(&prune));
    assert!(columns.iter().all(|column| !request.includes(column)));
}
