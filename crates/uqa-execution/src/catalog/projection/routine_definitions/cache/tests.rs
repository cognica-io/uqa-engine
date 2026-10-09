//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, CatalogServices};
use uqa_sql::{routines::RoutineBody, Statement};

fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    for position in 0..=unrelated {
        let Statement::CreateFunction(mut definition) = uqa_sql::compile(&format!("CREATE FUNCTION public.routine_{position:03}() RETURNS integer LANGUAGE SQL AS 'SELECT 1'")).unwrap().remove(0) else { panic!("routine"); };
        definition.catalog_oid = Some(70_000 + u32::try_from(position).unwrap());
        Arc::make_mut(&mut snapshot.definitions.sql_user_functions).insert(
            definition.name.clone(),
            vec![Arc::new(SQLUserFunction::new(
                *definition,
                RoutineBody::Source,
            ))],
        );
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn routine_inquiries_borrow_definitions_without_repeated_collection() {
    for unrelated in [0, 1, 128] {
        let catalog = fixture(unrelated);
        let expected = &catalog.snapshot().definitions.sql_user_functions["public.routine_000"][0];
        let before = ROUTINE_ADDRESSES.get();
        let owners = Arc::strong_count(expected);
        for _ in 0..4 {
            let alias = catalog.clone();
            assert!(std::ptr::eq(
                user_routine_by_oid(&alias, 70_000).unwrap().unwrap(),
                expected
            ));
            assert!(user_routine_by_oid(&alias, -1).unwrap().is_none());
        }
        assert_eq!(Arc::strong_count(expected), owners);
        assert_eq!(ROUTINE_ADDRESSES.get() - before, unrelated + 1);
    }
}

#[test]
fn concurrent_routine_inquiries_index_addresses_once() {
    let catalog = fixture(128);
    let ready = std::sync::Barrier::new(8);
    let reads = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let before = ROUTINE_ADDRESSES.get();
                    assert!(user_routine_by_oid(&catalog, 70_000).unwrap().is_some());
                    ROUTINE_ADDRESSES.get() - before
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(reads, 129);
}

#[test]
fn routine_address_errors_preserve_earlier_matches_and_precede_builtin_fallback() {
    let catalog = fixture(2);
    let mut snapshot = catalog.snapshot().clone();
    let routines = Arc::make_mut(&mut snapshot.definitions.sql_user_functions);
    Arc::make_mut(&mut routines.get_mut("public.routine_001").unwrap()[0])
        .def
        .catalog_oid = None;
    let catalog = CatalogReadView::new(snapshot);
    let services = CatalogServices::default();
    let output = crate::catalog::cache::RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let before = ROUTINE_ADDRESSES.get();
    assert!(user_routine_by_oid(&catalog, 70_000).unwrap().is_some());
    for oid in [70_001, 70_002, -1, 1255] {
        let Err(error) = user_routine_by_oid(&catalog, oid) else {
            panic!("invalid routine identity must precede a later match");
        };
        assert!(error
            .to_string()
            .contains("routine `public.routine_001` has no catalog object identity"));
    }
    let builtin = super::super::super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS[0][0];
    assert!(super::super::find_routine(&context, builtin.oid).is_err());
    assert_eq!(ROUTINE_ADDRESSES.get() - before, 2);
}

#[test]
fn routine_addresses_keep_overload_order_and_retained_generations() {
    let catalog = fixture(1);
    let original = user_routine_by_oid(&catalog, 70_000).unwrap().unwrap();
    let mut snapshot = catalog.snapshot().clone();
    let routines = Arc::make_mut(&mut snapshot.definitions.sql_user_functions);
    let mut first = routines.remove("public.routine_000").unwrap();
    Arc::make_mut(&mut first[0]).def.name = "public.renamed".into();
    let second = routines.remove("public.routine_001").unwrap();
    first.extend(second);
    routines.insert("public.renamed".into(), first);
    let current = CatalogReadView::new(snapshot.clone());
    assert_eq!(
        user_routine_by_oid(&current, 70_000)
            .unwrap()
            .unwrap()
            .def
            .name,
        "public.renamed"
    );
    assert_eq!(
        user_routine_by_oid(&current, 70_001)
            .unwrap()
            .unwrap()
            .def
            .name,
        "public.routine_001"
    );
    Arc::make_mut(&mut snapshot.definitions.sql_user_functions).clear();
    let removed = CatalogReadView::new(snapshot);
    assert!(user_routine_by_oid(&removed, 70_000).unwrap().is_none());
    assert_eq!(original.def.name, "public.routine_000");
    assert!(std::ptr::eq(
        original,
        user_routine_by_oid(&catalog, 70_000).unwrap().unwrap()
    ));
}
