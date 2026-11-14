//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::{catalog, resolution};
use super::*;
use crate::catalog::test_support::input::{advance, complete, Scope};
use uqa_core::{image_io::ImageDecoder, memory::RetainedValue};
use uqa_sql::catalog::{roles::RoleIdentity, security::BoundSchemaSecurity};

type Decoder = ImageDecoder<Produced<Vec<String>>, SQLError>;
fn decoder(
    scope: &Scope,
    catalog: CatalogReadView,
    path: RelationNameResolution,
    implicit: bool,
) -> Decoder {
    scope.decoder(|native, original, mut reader| async move {
        let mut input = reader.input();
        current_schema_names_from_input(
            &catalog,
            &path,
            &path.current_user,
            implicit,
            &ProductionControl::new(&native, &original, input.cancellation),
            &mut input,
        )
        .await
    })
}

#[test]
fn retained_namespace_selection_preserves_implicit_order_temporary_graphs_and_invalid_acl() {
    let mut snapshot = catalog().snapshot().clone();
    let mut invalid = BoundSchemaSecurity::with_public_privileges(true);
    invalid.acl.as_mut().unwrap().last_mut().unwrap().grantor = RoleIdentity {
        oid: 500,
        object_id: [50; 16],
    };
    RetainedValue::make_mut(&mut snapshot.definitions.schemas)
        .insert("invalid".into(), invalid)
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    let scope = Scope::default();
    for allocated in [false, true] {
        for implicit in [false, true] {
            for role in ["uqa", "reader", "missing"] {
                for names in [
                    vec![],
                    vec!["public"],
                    vec![
                        "invalid",
                        "missing",
                        "pg_temp",
                        "g",
                        "private",
                        "public",
                        "g",
                        "pg_catalog",
                        "information_schema",
                        "ag_catalog",
                        "pg_toast_temp_1",
                    ],
                ] {
                    let mut path = resolution(&names);
                    path.temporary_namespace_allocated = allocated;
                    path.current_user = role.into();
                    let expected =
                        super::super::current_schema_names(&catalog, &path, role, implicit);
                    for steps in [1, 7, 128] {
                        let mut input = decoder(&scope, catalog.clone(), path.clone(), implicit);
                        let actual = complete(&mut input, steps).unwrap().0;
                        assert_eq!(
                            *actual, expected,
                            "{role}, {allocated}, {implicit}, {names:?}"
                        );
                        assert!(advance(&mut input, 1).is_err());
                        assert_eq!(scope.native.used(), actual.reserved_bytes());
                        drop((actual, input));
                        scope.assert_released();
                    }
                }
            }
        }
    }
}

#[test]
fn retained_namespaces_bound_long_comparisons_copies_and_all_retirement_boundaries() {
    let name = format!("{}selected", "가".repeat(4097));
    let mut snapshot = catalog().snapshot().clone();
    RetainedValue::make_mut(&mut snapshot.definitions.schemas)
        .insert(
            name.clone(),
            BoundSchemaSecurity::with_public_privileges(true),
        )
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    let mut path = resolution(&["public", &name, &name]);
    path.current_user = "reader".into();
    let expected = super::super::current_schema_names(&catalog, &path, "reader", true);
    let scope = Scope::default();
    let (actual, polls) =
        complete(&mut decoder(&scope, catalog.clone(), path.clone(), true), 1).unwrap();
    assert_eq!(*actual, expected);
    assert!(polls >= name.len() / 4096 * 4);
    drop(actual);
    scope.assert_released();
    let mut denials = 0;
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop", "memory", "window"] {
            let scope = Scope::default();
            let mut input = decoder(&scope, catalog.clone(), path.clone(), true);
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            let hold = (action == "memory")
                .then(|| scope.native.reserve(scope.native.available()).unwrap());
            match action {
                "original" => scope.original.cancel(),
                "invoking" => scope.invoking.cancel(),
                _ => {}
            }
            if action != "drop" {
                let result = complete(&mut input, usize::from(action != "window"));
                if action == "memory" {
                    if let Err(error) = &result {
                        assert_eq!(error.sqlstate(), Some("53200"));
                        denials += 1;
                    }
                } else {
                    assert!(result.is_err());
                }
                drop(result);
            }
            drop((input, hold));
            scope.assert_released();
        }
    }
    assert!(denials > 0);
}
