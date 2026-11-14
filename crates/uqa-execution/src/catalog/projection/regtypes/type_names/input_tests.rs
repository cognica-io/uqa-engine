//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    test_support::{
        input::{advance, complete, Scope},
        type_names as fixtures,
    },
    CatalogReadView, RelationNameResolution,
};
use std::sync::Arc;
use uqa_core::{
    image_io::ImageDecoder,
    memory::{ProductionControl, RetainedValue},
    RelationIdentity,
};
use uqa_sql::{ast::UserTypeIdentity, SQLError};

mod reference;
mod resolution;
mod retention;

fn identity(ty: &ColumnType) -> Option<UserTypeIdentity> {
    ty.user_type_identity()
        .and_then(|name| UserTypeIdentity::parse(&name))
}
type Decoder = ImageDecoder<Option<UserTypeIdentity>, SQLError>;
fn qualified(scope: &Scope, catalog: CatalogReadView, schema: Arc<str>, name: Arc<str>) -> Decoder {
    scope.decoder(|native, original, mut reader| async move {
        let mut input = reader.input();
        catalog_user_type_name_identity_from_input(
            &catalog,
            &schema,
            &name,
            &ProductionControl::new(&native, &original, input.cancellation),
            &mut input,
        )
        .await
    })
}
fn routine(
    scope: &Scope,
    catalog: CatalogReadView,
    path: RelationNameResolution,
    name: Arc<str>,
) -> Decoder {
    scope.decoder(|native, original, mut reader| async move {
        let mut input = reader.input();
        routine_type_name_identity_from_input(
            &catalog,
            &path,
            &name,
            &ProductionControl::new(&native, &original, input.cancellation),
            &mut input,
        )
        .await
    })
}
fn compare(catalog: &CatalogReadView, schema: &str, name: &str) {
    let control = ProductionControl::uncontrolled();
    let expected = reference::select(catalog, schema, name, &control).unwrap();
    let actual = catalog_user_type_name_with_control(catalog, schema, name, &control).unwrap();
    assert_eq!(actual.as_deref(), expected.as_deref(), "{schema}.{name}");
    let scope = Scope::default();
    for steps in [1, 7, 128] {
        let mut input = qualified(&scope, catalog.clone(), schema.into(), name.into());
        assert_eq!(
            complete(&mut input, steps).unwrap().0,
            expected.as_deref().and_then(identity),
            "{schema}.{name}, {steps}"
        );
        assert!(advance(&mut input, 1).is_err());
        scope.assert_released();
    }
}

#[test]
fn retained_native_type_selection_matches_original_families_arrays_and_relation_order() {
    let catalog = fixtures::catalog();
    for variant in 0..6 {
        let catalog = collision_variant(&catalog, variant);
        for schema in ["public", "missing"] {
            for name in [
                "d",
                "_d",
                "e",
                "_e",
                "c",
                "_c",
                "r",
                "_r",
                "m",
                "_m",
                "t",
                "_t",
                "v",
                "view_array",
                "f",
                "foreign_array",
                "missing",
            ] {
                compare(&catalog, schema, name);
            }
        }
    }
}

fn collision_variant(catalog: &CatalogReadView, variant: usize) -> CatalogReadView {
    let mut snapshot = catalog.snapshot().clone();
    match variant {
        1 => {
            RetainedValue::make_mut(&mut snapshot.definitions.ranges)
                .edit("public.r", |value| {
                    value.array_name = "r".into();
                    value.multirange.array_name = "m".into();
                    Ok(())
                })
                .unwrap();
            RetainedValue::make_mut(&mut snapshot.definitions.domains)
                .edit("public.d", |value| {
                    value.array_name = Some("d".into());
                    Ok(())
                })
                .unwrap();
            RetainedValue::make_mut(&mut snapshot.definitions.enums)
                .edit("public.e", |value| {
                    value.array_name = "e".into();
                    Ok(())
                })
                .unwrap();
            RetainedValue::make_mut(&mut snapshot.definitions.composites)
                .edit("public.c", |value| {
                    value.array_name = "c".into();
                    Ok(())
                })
                .unwrap();
        }
        2 => {
            RetainedValue::make_mut(&mut snapshot.definitions.domains)
                .edit("public.d", |value| {
                    value.identity.name = "r".into();
                    Ok(())
                })
                .unwrap();
            RetainedValue::make_mut(&mut snapshot.definitions.enums)
                .edit("public.e", |value| {
                    value.identity.name = "r".into();
                    Ok(())
                })
                .unwrap();
        }
        3 => {
            RetainedValue::make_mut(&mut snapshot.definitions.enums)
                .edit("public.e", |value| {
                    value.identity.name = "d".into();
                    Ok(())
                })
                .unwrap();
            RetainedValue::make_mut(&mut snapshot.definitions.composites)
                .edit("public.c", |value| {
                    value.identity.name = "e".into();
                    Ok(())
                })
                .unwrap();
        }
        4 => {
            let mut table = snapshot
                .tables
                .remove(&RelationIdentity::new("public", "t"))
                .unwrap();
            table.row_type_array_name = Some("v".into()).into();
            snapshot
                .tables
                .insert(RelationIdentity::new("public", "f"), table);
        }
        5 => {
            let table = snapshot
                .tables
                .get_mut(&RelationIdentity::new("public", "t"))
                .unwrap();
            table.catalog_oids.row_type = None;
            table.row_type_array_name = Some("view_array".into()).into();
        }
        _ => {}
    }
    CatalogReadView::new(snapshot)
}
