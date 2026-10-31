//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn any_column_authority_preserves_whole_table_demand_and_column_order() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let memory = MemoryBudget::new(16 << 10);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.column_acls.insert(
        "id".into(),
        vec![entry(Some(roles["reader"].identity()), true)],
    );
    for subject in ["uqa", "reader"] {
        let subject = RoleReference::from(subject);
        let access = RelationPrivileges {
            subject: &subject,
            roles: &roles,
            memberships: &memberships,
            control,
        };
        let mut columns = 0;
        let allowed = access
            .any_column_for(&security, TableAclPrivilege::Select, |test| {
                columns += 1;
                assert!(!test("hidden")?);
                test("id")
            })
            .unwrap();
        assert!(allowed);
        assert_eq!(
            columns,
            usize::from(subject == RoleReference::from("reader"))
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn any_column_authority_preserves_catalog_errors_before_superuser_and_column_shortcuts() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let subject = RoleReference::from("uqa");
    let access = RelationPrivileges {
        subject: &subject,
        roles: &roles,
        memberships: &memberships,
        control,
    };
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let mut removed = roles["reader"].identity();
    removed.object_id = [72; 16];
    security
        .column_acls
        .insert("unread".into(), vec![entry(Some(removed), true)]);
    let error = access
        .any_column_for(&security, TableAclPrivilege::Select, |_| {
            panic!("invalid authority precedes column demand")
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn any_column_authority_releases_partial_membership_work_on_cancel_and_unwind() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let subject = RoleReference::from("reader");
    for failure in 0..3 {
        let memory = MemoryBudget::new(16 << 10);
        let sibling = memory.reserve(37).unwrap();
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&memory, &statement, &session);
        let access = RelationPrivileges {
            subject: &subject,
            roles: &roles,
            memberships: &memberships,
            control,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            access.any_column_for(&security, TableAclPrivilege::Select, |test| {
                assert!(!test("first")?);
                match failure {
                    0 => statement.cancel(),
                    1 => session.cancel(),
                    _ => panic!("column observation unwind"),
                }
                test("second")
            })
        }));
        if failure == 2 {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        }
        assert_eq!(memory.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}
