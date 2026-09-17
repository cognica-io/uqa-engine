//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn table_lock_privileges_follow_postgresql_18_relation_acl_masks() {
    use TableLockMode::*;
    let modes = [
        AccessShare,
        RowShare,
        RowExclusive,
        ShareUpdateExclusive,
        Share,
        ShareRowExclusive,
        Exclusive,
        AccessExclusive,
    ];
    for (privilege, expected) in [
        (TableAclPrivilege::Select, "Y......."),
        (TableAclPrivilege::Insert, "YYY....."),
        (TableAclPrivilege::Update, "YYYYYYYY"),
        (TableAclPrivilege::Delete, "YYYYYYYY"),
        (TableAclPrivilege::Truncate, "YYYYYYYY"),
        (TableAclPrivilege::Maintain, "YYYYYYYY"),
        (TableAclPrivilege::References, "........"),
        (TableAclPrivilege::Trigger, "........"),
    ] {
        for (mode, allowed) in modes.into_iter().zip(expected.bytes()) {
            assert_eq!(
                lock_privilege_permits(privilege, mode),
                allowed == b'Y',
                "{privilege:?}, {mode:?}"
            );
        }
    }
}

#[test]
fn view_lock_targets_preserve_only_scope_and_skip_ctes_through_nested_queries() {
    let query = crate::compile("WITH RECURSIVE a AS (SELECT * FROM b), b AS (SELECT id FROM ONLY public.parent) SELECT a.id, (SELECT max(id) FROM public.scalar_source) FROM a JOIN (SELECT * FROM public.children) c ON a.id = c.id UNION ALL SELECT id, id FROM public.tail").unwrap().remove(0);
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(query) else {
        panic!("expected query")
    };
    let targets = view_lock_targets(&query).unwrap();
    assert_eq!(
        targets,
        vec![
            LockTableTarget {
                name: "public.parent".into(),
                include_descendants: false
            },
            LockTableTarget {
                name: "public.children".into(),
                include_descendants: true
            },
            LockTableTarget {
                name: "public.scalar_source".into(),
                include_descendants: true
            },
            LockTableTarget {
                name: "public.tail".into(),
                include_descendants: true
            },
        ]
    );
    assert!(
        !query.relations_bound,
        "target collection must not bind the stored plan"
    );
}
