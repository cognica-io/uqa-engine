//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::projection::{
    build_requested_catalog_rows, information_schema::COLUMN_ROWS,
    view_definition::EXPRESSION_OUTPUTS, CatalogRequest,
};
use uqa_sql::{ast::BinaryOp, ScalarExpr};

fn equal(qualifier: &str, column: &str, value: &str) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: qualifier.into(),
            column: column.into(),
        }),
        rhs: Box::new(ScalarExpr::Literal(Value::Str(value.into()))),
    }
}

fn request(
    catalog: &CatalogReadView,
    source: &str,
    alias: &str,
    names: &[(&str, &str)],
) -> CatalogRequest {
    let columns = crate::catalog::schema::virtual_relation_schema(
        catalog,
        &CatalogServices::default().resolution,
        source,
    )
    .unwrap()
    .unwrap()
    .into_iter()
    .map(|(name, _)| name)
    .collect::<Vec<_>>();
    let filters = BTreeMap::from([(
        alias.into(),
        vec![ScalarExpr::And(
            names
                .iter()
                .map(|(name, value)| equal(alias, name, value))
                .collect(),
        )],
    )]);
    crate::query::source_projection::catalog_request(&columns, &[], alias, None, Some(&filters))
}

fn project(
    catalog: &CatalogReadView,
    source: &str,
    request: &CatalogRequest,
) -> Vec<uqa_sql::ResultRow> {
    let services = CatalogServices::default();
    let output = RegtypeOutputCache::default();
    build_requested_catalog_rows(
        &services.context(catalog, &output),
        catalog,
        &services.resolution,
        &services,
        source,
        request,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn catalog_name_bounds_limit_column_rows_and_expression_work() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let before = (
            COLUMN_ROWS.get(),
            EXPRESSION_OUTPUTS.get(),
            OUTPUT_METADATA_BUILDS.get(),
        );
        for alias in ["a", "b", "a"] {
            let names = [
                ("table_schema", "public"),
                ("table_name", "items_000"),
                ("column_name", "type_ref"),
            ];
            let rows = project(
                &catalog,
                "information_schema.columns",
                &request(&catalog, "information_schema.columns", alias, &names),
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0]["column_default"],
                Value::Str("'hidden.positive'::regtype".into())
            );
            assert_eq!(rows[0]["ordinal_position"], Value::Int(1));
        }
        assert_eq!(COLUMN_ROWS.get() - before.0, 3);
        assert_eq!(EXPRESSION_OUTPUTS.get() - before.1, 3);
        assert_eq!(OUTPUT_METADATA_BUILDS.get() - before.2, 1);
        let catalog = fixture(unrelated);
        let before = (
            COLUMN_ROWS.get(),
            EXPRESSION_OUTPUTS.get(),
            OUTPUT_METADATA_BUILDS.get(),
        );
        for names in [
            vec![("table_name", "missing")],
            vec![("table_schema", "missing")],
            vec![("column_name", "missing")],
            vec![("table_name", "items_000"), ("table_name", "items_001")],
        ] {
            assert!(project(
                &catalog,
                "information_schema.columns",
                &request(&catalog, "information_schema.columns", "a", &names)
            )
            .is_empty());
        }
        assert_eq!(COLUMN_ROWS.get(), before.0);
        assert_eq!(EXPRESSION_OUTPUTS.get(), before.1);
        assert_eq!(OUTPUT_METADATA_BUILDS.get(), before.2);
    }
}

fn defaults_fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = fixture(0).snapshot().clone();
    let domain = Arc::make_mut(&mut snapshot.definitions.domains)
        .get_mut("hidden.positive")
        .unwrap();
    domain.definition.base = ColumnType::Regtype;
    domain.definition.default = Some(constant(25, "regtype"));
    let domain = domain.clone();
    let function = Arc::make_mut(
        &mut Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
            .get_mut("hidden.echo")
            .unwrap()[0],
    );
    function.def.params[0].default = Some(constant(60_000, "regtype"));
    let function = function.clone();
    for i in 0..unrelated {
        let mut domain = domain.clone();
        domain.identity.name = format!("unrelated_{i}");
        domain.definition.name = domain.identity.qualified_name();
        domain.oid = 80_000 + u32::try_from(i).unwrap() * 2;
        domain.array_oid = Some(domain.oid + 1);
        domain.object_id = (i as u128 + 500).to_le_bytes();
        Arc::make_mut(&mut snapshot.definitions.domains)
            .insert(domain.definition.name.clone(), domain);
        let mut function = function.clone();
        function.def.name = format!("hidden.unrelated_{i}");
        function.def.catalog_oid = Some(90_000 + u32::try_from(i).unwrap());
        Arc::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(function.def.name.clone(), vec![Arc::new(function)]);
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn catalog_name_bounds_only_deparse_matching_domain_and_routine_defaults() {
    for unrelated in [0, 1, 128] {
        for (source, column, name, output) in [
            ("pg_catalog.pg_type", "typname", "positive", "typdefault"),
            ("pg_catalog.pg_proc", "proname", "echo", "proargdefaults"),
        ] {
            let catalog = defaults_fixture(unrelated);
            let before = (EXPRESSION_OUTPUTS.get(), OUTPUT_METADATA_BUILDS.get());
            let rows = project(
                &catalog,
                source,
                &request(&catalog, source, "c", &[(column, name)]),
            );
            assert_eq!(rows.len(), 1);
            assert_ne!(rows[0][output], Value::Null);
            assert_eq!(EXPRESSION_OUTPUTS.get() - before.0, 1);
            assert_eq!(OUTPUT_METADATA_BUILDS.get() - before.1, 1);
            let catalog = defaults_fixture(unrelated);
            let before = (EXPRESSION_OUTPUTS.get(), OUTPUT_METADATA_BUILDS.get());
            assert!(project(
                &catalog,
                source,
                &request(&catalog, source, "c", &[(column, "missing")])
            )
            .is_empty());
            assert_eq!(EXPRESSION_OUTPUTS.get(), before.0);
            assert_eq!(OUTPUT_METADATA_BUILDS.get(), before.1);
        }
    }
}

#[test]
fn column_name_bounds_keep_original_ordinals_and_retained_generations() {
    let old = fixture(0);
    let old_request = request(
        &old,
        "information_schema.columns",
        "c",
        &[("column_name", "proc_ref")],
    );
    let rows = project(&old, "information_schema.columns", &old_request);
    assert_eq!(rows[0]["ordinal_position"], Value::Int(2));
    let mut snapshot = old.snapshot().clone();
    let table = snapshot
        .tables
        .remove(&RelationIdentity::new("public", "items_000"))
        .unwrap();
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "renamed"), table);
    let new = CatalogReadView::new(snapshot);
    let names = [("table_name", "items_000")];
    assert!(project(
        &new,
        "information_schema.columns",
        &request(&new, "information_schema.columns", "c", &names)
    )
    .is_empty());
    assert_eq!(
        project(
            &old,
            "information_schema.columns",
            &request(&old, "information_schema.columns", "c", &names)
        )
        .len(),
        2
    );
}

#[test]
fn column_bounds_skip_unrelated_views_and_unrequested_mutation_analysis() {
    use uqa_sql::catalog::{
        security::BoundTableSecurity, stored_view::StoredViewDefinition, view::StoredViewKind,
    };
    let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT 1 AS visible_column")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let mut snapshot = fixture(0).snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.views).insert(
        RelationIdentity::new("public", "visible_view"),
        crate::catalog::view::StoredView {
            security: BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
            definition: StoredViewDefinition {
                object_id: [4; 16],
                query: *query,
                output_columns: Some(vec!["visible_column".into()]),
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
                options: Vec::new(),
                kind: StoredViewKind::View,
                materialized_rows: Vec::new(),
                materialized_column_types: Vec::new(),
                populated: true,
                catalog_oids: Some(uqa_sql::catalog::relation_oids::RelationCatalogOids {
                    relation: 100_000,
                    row_type: Some(100_001),
                    array_type: Some(100_002),
                    rule: Some(100_003),
                }),
                row_type_array_name: None,
            },
        },
    );
    let catalog = CatalogReadView::new(snapshot);
    let selected = request(
        &catalog,
        "information_schema.columns",
        "c",
        &[("table_name", "items_000")],
    );
    assert_eq!(
        project(&catalog, "information_schema.columns", &selected).len(),
        2
    );
    let mut request = CatalogRequest::columns(["column_name".into()]);
    request.require_name("table_name".into(), "visible_view".into());
    let rows = project(&catalog, "information_schema.columns", &request);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["column_name"], Value::Str("visible_column".into()));
}
