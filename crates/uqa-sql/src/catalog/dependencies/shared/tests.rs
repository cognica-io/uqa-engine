//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{NAMESPACE_CLASS, RELATION_CLASS, ROLE_CLASS, TYPE_CLASS};
use super::*;

fn role(oid: u32) -> ObjectAddress {
    ObjectAddress::whole(ROLE_CLASS, oid)
}

fn dependency(
    dependent: ObjectAddress,
    referenced: ObjectAddress,
    kind: SharedDependencyKind,
) -> SharedDependency {
    SharedDependency {
        database: 5,
        dependent,
        referenced,
        kind,
    }
}

fn describe(object: ObjectAddress) -> Result<Option<String>, crate::SQLError> {
    Ok(Some(
        match (object.class_id, object.object_id, object.sub_id) {
            (NAMESPACE_CLASS, 2200, 0) => "schema public".into(),
            (NAMESPACE_CLASS, 16_390, 0) => "schema other".into(),
            (TYPE_CLASS, 16_385, 0) => "type other.feeling".into(),
            (TYPE_CLASS, 16_388, 0) => "type other.posint2".into(),
            (RELATION_CLASS, 16_392, 0) => "table lt".into(),
            (RELATION_CLASS, 16_392, 1) => "column id of table lt".into(),
            (RELATION_CLASS, oid, 0) => format!("table t{oid}"),
            _ => return Ok(None),
        },
    ))
}

#[test]
fn objects_are_listed_by_oid_whatever_their_catalog() {
    use SharedDependencyKind::Acl;
    let user = role(16_400);
    let rows = [
        dependency(ObjectAddress::whole(NAMESPACE_CLASS, 16_390), user, Acl),
        dependency(ObjectAddress::whole(TYPE_CLASS, 16_388), user, Acl),
        dependency(ObjectAddress::whole(NAMESPACE_CLASS, 2200), user, Acl),
        dependency(ObjectAddress::whole(TYPE_CLASS, 16_385), user, Acl),
        // Another role's rows are not reported.
        dependency(ObjectAddress::whole(TYPE_CLASS, 16_385), role(16_401), Acl),
    ];
    assert_eq!(
        shared_dependency_detail(&rows, user, &describe).unwrap().as_deref(),
        Some(
            "privileges for schema public\nprivileges for type other.feeling\nprivileges for type other.posint2\nprivileges for schema other"
        )
    );
}

#[test]
fn a_relation_precedes_its_columns_and_privileges_precede_ownership() {
    use SharedDependencyKind::{Acl, Owner};
    let owner = role(16_400);
    let rows = [
        dependency(ObjectAddress::column(16_392, 1), owner, Acl),
        dependency(ObjectAddress::whole(RELATION_CLASS, 16_392), owner, Owner),
        dependency(ObjectAddress::whole(RELATION_CLASS, 16_392), owner, Acl),
    ];
    assert_eq!(
        shared_dependency_detail(&rows, owner, &describe)
            .unwrap()
            .as_deref(),
        Some("privileges for table lt\nowner of table lt\nprivileges for column id of table lt")
    );
}

#[test]
fn nothing_depends_on_a_role_without_rows() {
    assert_eq!(
        shared_dependency_detail(&[], role(16_400), &describe).unwrap(),
        None
    );
}

#[test]
fn the_detail_lists_one_hundred_objects() {
    let owner = role(16_400);
    let rows = (0..103)
        .map(|index| {
            dependency(
                ObjectAddress::whole(RELATION_CLASS, 20_000 + index),
                owner,
                SharedDependencyKind::Owner,
            )
        })
        .collect::<Vec<_>>();
    let detail = shared_dependency_detail(&rows, owner, &describe)
        .unwrap()
        .expect("dependents");
    let lines = detail.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 101);
    assert_eq!(lines[0], "owner of table t20000");
    assert_eq!(lines[99], "owner of table t20099");
    assert_eq!(lines[100], "and 3 other objects (see server log for list)");
}
