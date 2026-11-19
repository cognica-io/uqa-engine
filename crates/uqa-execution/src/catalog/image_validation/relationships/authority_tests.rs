//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Predicate, RelationshipValidation, Scheduled};
use crate::catalog::{
    image_validation::{tests::snapshot, validate_authority},
    test_support::NoRoutines,
    CatalogReadView,
};
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken,
};
use uqa_sql::{
    ast::{CreateFunction, ObjectAclEntry},
    catalog::roles::{RoleDefinition, RoleIdentity},
    routines::{RoutineBody, SQLUserFunction},
    SQLError,
};

const COUNT: u32 = 4096;

fn definition() -> CreateFunction {
    let uqa_sql::Statement::CreateFunction(definition) =
        uqa_sql::compile("CREATE FUNCTION check_owner() RETURNS integer LANGUAGE SQL RETURN 0")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    *definition
}

fn routines() -> CatalogReadView {
    let mut snapshot = snapshot();
    let template = definition();
    let registry = RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions);
    for row in 1..=COUNT {
        let mut item = template.clone();
        item.name = format!("public.r{row:05}");
        item.catalog_oid = Some(20_000 + row);
        item.object_id = Some(u128::from(row).to_be_bytes());
        item.owner = (row != COUNT).then_some(RoleIdentity::BOOTSTRAP);
        if item.owner.is_none() {
            item.execute_acl = Some(vec![ObjectAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                grant_option: true,
            }]);
        }
        registry
            .insert(
                item.name.clone(),
                vec![SQLUserFunction::new(item, RoutineBody::Source).shared()],
            )
            .unwrap();
    }
    CatalogReadView::new(snapshot)
}

#[derive(Clone, Copy)]
enum GrantFixture {
    Valid,
    MissingOwner,
    DetachedGrant,
}

fn grants(fixture: GrantFixture) -> CatalogReadView {
    let mut snapshot = snapshot();
    let roles = RetainedValue::make_mut(&mut snapshot.definitions.roles);
    let mut acl = Vec::new();
    let mut previous = RoleIdentity::BOOTSTRAP;
    for row in 1..=COUNT {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("member_{row:05}");
        role.oid = i64::from(20_000 + row);
        role.object_id = u128::from(50_000 + row).to_be_bytes();
        let identity = role.identity();
        roles.insert(role.name.clone(), role).unwrap();
        acl.push(ObjectAclEntry {
            role: Some(identity),
            grantor: if row == COUNT && matches!(fixture, GrantFixture::DetachedGrant) {
                identity
            } else {
                previous
            },
            grant_option: true,
        });
        previous = identity;
    }
    let mut item = definition();
    item.name = "public.single_acl".into();
    item.catalog_oid = Some(30_000);
    item.object_id = Some(900_000_u128.to_be_bytes());
    item.owner =
        (!matches!(fixture, GrantFixture::MissingOwner)).then_some(RoleIdentity::BOOTSTRAP);
    item.execute_acl = Some(acl);
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert(
            item.name.clone(),
            vec![SQLUserFunction::new(item, RoutineBody::Source).shared()],
        )
        .unwrap();
    CatalogReadView::new(snapshot)
}

fn enter(
    view: &CatalogReadView,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> RelationshipValidation {
    let mut cursor = RelationshipValidation::new(view.clone(), memory, cancellation);
    loop {
        if matches!(
            cursor.remaining.first(),
            Some(Predicate::Scheduled(Scheduled::Authority))
        ) {
            return cursor;
        }
        cursor = cursor
            .advance_with_steps(&NoRoutines, &NoRoutines, 256)
            .unwrap()
            .expect("authority follows namespace and name identity");
    }
}

fn prefix(mut cursor: RelationshipValidation, windows: usize) -> RelationshipValidation {
    for _ in 0..windows {
        cursor = cursor
            .advance(&NoRoutines, &NoRoutines)
            .unwrap()
            .expect("more authority rows");
        assert!(matches!(
            cursor.remaining.first(),
            Some(Predicate::Scheduled(Scheduled::Authority))
        ));
    }
    cursor
}

fn counted_terminal(mut cursor: RelationshipValidation, steps: usize) -> (usize, SQLError) {
    for window in 1..=(usize::try_from(COUNT).unwrap() * 30) {
        match cursor.advance_with_steps(&NoRoutines, &NoRoutines, steps) {
            Ok(Some(next)) => cursor = next,
            Ok(None) => panic!("invalid catalog accepted"),
            Err(error) => return (window, error),
        }
    }
    panic!("authority row validation did not terminate")
}

fn terminal(cursor: RelationshipValidation, steps: usize) -> SQLError {
    counted_terminal(cursor, steps).1
}

#[test]
fn one_step_relationship_validation_reaches_last_missing_routine_owner_in_order() {
    let view = routines();
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    let expected = validate_authority(view.snapshot(), &memory, &cancel)
        .unwrap_err()
        .to_string();
    assert!(expected.contains("invalid native catalog routine owner"));
    let cursor = prefix(enter(&view, &memory, &cancel), COUNT as usize);
    assert!(memory.used() > 0);
    assert_eq!(terminal(cursor, 64).to_string(), expected);
    assert_eq!(memory.used(), 0);
}

#[test]
fn one_step_relationship_validation_suspends_within_one_routine_grant_graph() {
    let view = grants(GrantFixture::DetachedGrant);
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    let expected = validate_authority(view.snapshot(), &memory, &cancel)
        .unwrap_err()
        .to_string();
    assert!(expected.contains("ACL grantor has no owner-rooted grant option"));
    let prior = grants(GrantFixture::MissingOwner);
    let (before_grants, missing) = counted_terminal(enter(&prior, &memory, &cancel), 1);
    assert!(missing.to_string().contains("routine owner"));
    let cursor = prefix(
        enter(&view, &memory, &cancel),
        before_grants + COUNT as usize * 3,
    );
    assert_eq!(terminal(cursor, 1).to_string(), expected);
    assert_eq!(memory.used(), 0);
}

#[test]
fn suspended_relationship_authority_retires_on_pressure_cancellation_and_drop() {
    let view = grants(GrantFixture::DetachedGrant);
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    let cursor = prefix(enter(&view, &memory, &cancel), 64);
    drop(cursor);
    assert_eq!(memory.used(), 0);

    let cursor = prefix(enter(&view, &memory, &cancel), 64);
    let available = memory.limit() - memory.used();
    let pressure = memory.reserve(available).unwrap();
    assert_eq!(terminal(cursor, 1).sqlstate(), Some("53200"));
    assert_eq!(memory.used(), available);
    drop(pressure);
    assert_eq!(memory.used(), 0);

    let cursor = prefix(enter(&view, &memory, &cancel), COUNT as usize * 6);
    cancel.cancel();
    assert_eq!(terminal(cursor, 1).sqlstate(), Some("57014"));
    assert_eq!(memory.used(), 0);
    let recovered =
        validate_authority(view.snapshot(), &memory, &CancellationToken::new()).unwrap_err();
    assert!(recovered
        .to_string()
        .contains("ACL grantor has no owner-rooted grant option"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn valid_large_routine_grants_retire_the_final_role_index_across_work_windows() {
    let view = grants(GrantFixture::Valid);
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    validate_authority(view.snapshot(), &memory, &cancel).unwrap();
    let mut cursor = enter(&view, &memory, &cancel);
    let mut previous = memory.used();
    let mut decreasing = 0;
    for _ in 0..COUNT as usize * 30 {
        cursor = cursor
            .advance(&NoRoutines, &NoRoutines)
            .unwrap()
            .expect("foreign predicate follows authority");
        let now = memory.used();
        if now < previous {
            decreasing += 1;
        } else {
            decreasing = 0;
        }
        previous = now;
        if !matches!(
            cursor.remaining.first(),
            Some(Predicate::Scheduled(Scheduled::Authority))
        ) {
            assert!(
                decreasing >= COUNT as usize,
                "final role index retired in {decreasing} shrinking windows"
            );
            assert_eq!(now, 0);
            return;
        }
    }
    panic!("valid grant graph did not finish");
}
