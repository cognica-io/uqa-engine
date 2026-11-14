//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::UserTypeKind;

#[test]
fn retained_type_lookup_observes_both_cancellation_owners_even_for_empty_catalogs() {
    for original_cancelled in [false, true] {
        let scope = Scope::default();
        let mut input = qualified(
            &scope,
            crate::catalog::test_support::empty_catalog(),
            "public".into(),
            "missing".into(),
        );
        if original_cancelled {
            scope.original.cancel();
        } else {
            scope.invoking.cancel();
        }
        assert_eq!(
            advance(&mut input, 1).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert!(advance(&mut input, 1).is_err());
        scope.assert_released();
    }
}

#[test]
fn retained_type_identity_selection_borrows_wide_names_and_domain_bases_without_copying() {
    let schema: Arc<str> = "가".repeat(8193).into();
    let name: Arc<str> = "나".repeat(8193).into();
    let mut snapshot = fixtures::catalog().snapshot().clone();
    let mut domain = fixtures::domain(&schema, &name);
    domain.definition.base = ColumnType::Domain {
        schema: "public".into(),
        name: "deep_payload".repeat(100_000),
        oid: 99,
        array_oid: None,
        collation_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    RetainedValue::make_mut(&mut snapshot.definitions.domains)
        .insert("wide".into(), domain)
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    let scope = Scope {
        native: uqa_core::memory::MemoryBudget::new(0),
        ..Scope::default()
    };
    let mut input = qualified(
        &scope,
        catalog.clone(),
        Arc::clone(&schema),
        Arc::clone(&name),
    );
    let mut changed = catalog.snapshot().clone();
    RetainedValue::make_mut(&mut changed.definitions.domains).clear();
    drop((catalog, changed));
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(complete(&mut input, 1).unwrap());
    });
    assert_eq!(allocation.bytes_total, 0);
    let (value, polls) = result.unwrap();
    assert_eq!(
        value,
        Some(UserTypeIdentity {
            kind: UserTypeKind::Domain,
            oid: 41000,
            dimensions: 0
        })
    );
    assert!(polls >= (schema.len() + name.len()) / 4096);
    scope.assert_released();
}

#[test]
fn retained_catalog_type_lookup_retires_every_pending_parser_namespace_and_selection_state() {
    let catalog = fixtures::catalog();
    let path = crate::catalog::test_support::CatalogServices::default().resolution;
    let scope = Scope::default();
    let polls = complete(
        &mut routine(
            &scope,
            catalog.clone(),
            path.clone(),
            "foreign_array[][]".into(),
        ),
        1,
    )
    .unwrap()
    .1;
    assert!(polls > 25);
    let mut denials = 0;
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop", "memory", "window"] {
            let scope = Scope::default();
            let mut input = routine(
                &scope,
                catalog.clone(),
                path.clone(),
                "foreign_array[][]".into(),
            );
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
