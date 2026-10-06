//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::identity::RoleSubject;

#[test]
fn builtin_acl_validation_preserves_role_identity_across_rename() {
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_001;
    reader.object_id = [9; 16];
    reader.attributes.clear();
    let identity = reader.identity();
    let mut roles = BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader.clone()),
    ]);
    let entry = BuiltinRoutineSecurity {
        revision: [7; 16],
        execute_acl: vec![RoutineAclEntry {
            role: Some(identity),
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: false,
        }],
    };
    validate(&entry, &roles).unwrap();
    roles.remove("reader");
    reader.name = "renamed".into();
    roles.insert("renamed".into(), reader);
    validate(&entry, &roles).unwrap();
    assert_eq!(identity.role_name(&roles), Some("renamed"));
    roles.get_mut("renamed").unwrap().object_id = [8; 16];
    assert!(validate(&entry, &roles).is_err());
}

#[test]
fn builtin_acl_validation_never_substitutes_default_public_for_corruption() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let empty = BuiltinRoutineSecurity {
        revision: [1; 16],
        execute_acl: vec![],
    };
    validate(&empty, &roles).unwrap();
    assert!(!object_acl::privilege_allowed(
        &RoleIdentity::BOOTSTRAP,
        Some(&empty.execute_acl),
        false,
        false,
        |_| false
    ));
    let mut invalid = empty;
    invalid.revision = [0; 16];
    assert!(validate(&invalid, &roles).is_err());
    invalid.revision = [1; 16];
    invalid.execute_acl.push(RoutineAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        grant_option: true,
    });
    assert!(validate(&invalid, &roles).is_err());
}

#[test]
fn executable_initialization_excludes_dead_scalar_arms_but_keeps_unknown_conditions() {
    for (sql, names) in [
        ("SELECT CASE WHEN false THEN lower(v) ELSE 'ok' END", vec![]),
        (
            "SELECT CASE 1 WHEN 2 THEN lower(v) WHEN 1 THEN upper(v) ELSE lower(v) END",
            vec!["upper"],
        ),
        ("SELECT coalesce('ok',lower(v))", vec!["coalesce"]),
        (
            "SELECT coalesce(NULL,v,lower(v))",
            vec!["coalesce", "lower"],
        ),
        (
            "SELECT CASE WHEN flag THEN lower(v) ELSE upper(v) END",
            vec!["lower", "upper"],
        ),
    ] {
        let crate::Statement::Select(mut select) = crate::compile(sql).unwrap().remove(0) else {
            panic!("SELECT")
        };
        let expression =
            crate::plan::ExpressionPlan::lower(select.projections.remove(0).expr).scalar;
        let mut actual = Vec::new();
        initialization::visit(&expression, &mut |part| {
            if let crate::ScalarExpr::Func { name, .. } = part {
                actual.push(name.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(actual, names, "{sql}");
    }
}

#[test]
fn aggregate_permission_identity_uses_sql_overload_ranking_and_implicit_casts() {
    let identities = [
        BuiltinRoutineIdentity {
            oid: 2107,
            name: "sum",
            kind: 'a',
            argument_types: &[20],
        },
        BuiltinRoutineIdentity {
            oid: 2108,
            name: "sum",
            kind: 'a',
            argument_types: &[23],
        },
        BuiltinRoutineIdentity {
            oid: 2111,
            name: "sum",
            kind: 'a',
            argument_types: &[701],
        },
    ];
    for (argument, oid) in [
        (crate::ColumnType::Integer, 2108),
        (crate::ColumnType::BigInteger, 2107),
        (crate::ColumnType::Real, 2111),
    ] {
        let selected = selection::select_set_call(
            "pg_catalog.sum",
            &[Some(argument)],
            false,
            identities.into_iter(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.oid, oid);
    }
    assert!(selection::select_set_call(
        "app.sum",
        &[Some(crate::ColumnType::Integer)],
        false,
        identities.into_iter()
    )
    .unwrap()
    .is_none());
}
