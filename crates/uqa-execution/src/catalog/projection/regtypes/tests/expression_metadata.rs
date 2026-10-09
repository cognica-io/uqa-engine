//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    test_support::{empty_catalog, table_snapshot, CatalogServices},
};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, Expr, TableConstraintSet},
    catalog::{domain::StoredDomain, roles::RoleDefinition, security::BoundSchemaSecurity},
    routines::{RoutineBody, SQLUserFunction},
    Statement,
};

mod lifecycle;

fn constant(oid: i64, ty: &str) -> Expr {
    Expr::TypedLiteral {
        value: Value::Int(oid),
        ty: ty.into(),
        composite_source: None,
    }
}

fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [9; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader),
    ]));
    let mut schemas = BoundSchemaSecurity::initial_catalog();
    schemas.insert(
        "hidden".into(),
        BoundSchemaSecurity::owner(RoleIdentity::BOOTSTRAP),
    );
    snapshot.definitions.schemas = Arc::new(schemas);
    let Statement::CreateDomain(definition) =
        uqa_sql::compile("CREATE DOMAIN hidden.positive AS integer DEFAULT abs(-7)")
            .unwrap()
            .remove(0)
    else {
        panic!("domain");
    };
    snapshot.definitions.domains = Arc::new(BTreeMap::from([(
        "hidden.positive".into(),
        StoredDomain {
            object_id: [2; 16],
            oid: 60_000,
            array_oid: Some(60_001),
            identity: RelationIdentity::new("hidden", "positive"),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
            array_name: None,
            usage_acl: None,
        },
    )]));
    let Statement::CreateFunction(mut definition) = uqa_sql::compile("CREATE FUNCTION hidden.echo(n regtype DEFAULT 'hidden.positive'::regtype) RETURNS regtype LANGUAGE SQL AS 'SELECT n'").unwrap().remove(0) else { panic!("function"); };
    definition.catalog_oid = Some(70_000);
    definition.owner = Some(RoleIdentity::BOOTSTRAP);
    snapshot.definitions.sql_user_functions = Arc::new(BTreeMap::from([(
        "hidden.echo".into(),
        vec![Arc::new(SQLUserFunction::new(
            *definition,
            RoutineBody::Source,
        ))],
    )]));
    for position in 0..=unrelated {
        let mut type_ref = ColumnDef::nullable("type_ref", ColumnType::Regtype);
        type_ref.default = Some(constant(60_000, "regtype"));
        let mut proc_ref = ColumnDef::nullable("proc_ref", ColumnType::Regprocedure);
        proc_ref.default = Some(constant(70_000, "regprocedure"));
        snapshot.tables.insert(
            RelationIdentity::new("public", format!("items_{position:03}")),
            table_snapshot(
                (position as u128 + 10).to_le_bytes(),
                vec![type_ref, proc_ref],
                TableConstraintSet::default(),
            ),
        );
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn stored_expression_outputs_share_one_catalog_across_columns_and_aliases() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let services = CatalogServices::default();
        let output = RegtypeOutputCache::default();
        let context = services.context(&catalog, &output);
        let before = OUTPUT_METADATA_BUILDS.get();
        for _ in 0..4 {
            let alias = catalog.clone();
            let rows = crate::catalog::projection::information_schema::build_info_columns(
                &context,
                &alias,
                &services.resolution,
            )
            .unwrap();
            assert_eq!(rows.len(), 2 * (unrelated + 1));
            for row in rows {
                let expected = if row["column_name"] == Value::Str("type_ref".into()) {
                    "'hidden.positive'::regtype"
                } else {
                    "'hidden.echo(regtype)'::regprocedure"
                };
                assert_eq!(row["column_default"], Value::Str(expected.into()));
            }
        }
        assert_eq!(OUTPUT_METADATA_BUILDS.get() - before, 1);
    }
}

#[test]
fn concurrent_expression_outputs_build_alias_metadata_once() {
    let catalog = fixture(128);
    let resolution = CatalogServices::default().resolution;
    let ready = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = OUTPUT_METADATA_BUILDS.get();
                    let output = AliasConstantOutput::build(&catalog, &resolution).unwrap();
                    assert_eq!(
                        output.text(&ColumnType::Regtype, 60_000),
                        Some("hidden.positive".into())
                    );
                    (output.catalog, OUTPUT_METADATA_BUILDS.get() - before)
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().map(|(_, builds)| builds).sum::<usize>(), 1);
    for (catalog, _) in &results {
        assert!(Arc::ptr_eq(catalog, &results[0].0));
    }
}

#[test]
fn expression_output_metadata_keeps_visibility_outside_the_shared_catalog() {
    let catalog = fixture(0);
    let mut resolution = CatalogServices::default().resolution;
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Dynamic);
    resolution.search_path = vec!["hidden".into(), "public".into()];
    let before = OUTPUT_METADATA_BUILDS.get();
    let owner = AliasConstantOutput::build(&catalog, &resolution).unwrap();
    assert_eq!(
        owner.text(&ColumnType::Regtype, 60_000),
        Some("positive".into())
    );
    resolution.current_user = "reader".into();
    let reader = AliasConstantOutput::build(&catalog, &resolution).unwrap();
    assert_eq!(
        reader.text(&ColumnType::Regtype, 60_000),
        Some("hidden.positive".into())
    );
    assert!(Arc::ptr_eq(&owner.catalog, &reader.catalog));
    resolution.current_user = "uqa".into();
    resolution.search_path = vec!["public".into()];
    let public = AliasConstantOutput::build(&catalog, &resolution).unwrap();
    assert_eq!(
        public.text(&ColumnType::Regprocedure, 70_000),
        Some("hidden.echo(regtype)".into())
    );
    assert_eq!(OUTPUT_METADATA_BUILDS.get() - before, 1);
}
