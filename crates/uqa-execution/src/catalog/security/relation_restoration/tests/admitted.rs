//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_acl::{BoundRelationSecurity, RelationAclRevisions, TablePrivileges},
    catalog_role::{BoundAclEntry, RoleIdentity},
    memory::MemoryBudget,
    CancellationToken,
};

mod continuation;

fn admitted(memory: &MemoryBudget) -> Budgeted<RelationSecurityRow> {
    let token = CancellationToken::new();
    let entry = BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges {
            select: true,
            ..Default::default()
        },
        grant_options: TablePrivileges::default(),
    };
    let acl = uqa_core::catalog_acl::input::entries(
        &serde_json::to_vec(std::slice::from_ref(&entry)).unwrap(),
        memory,
        &token,
    )
    .unwrap();
    let columns = uqa_core::catalog_acl::input::column_entries(
        &serde_json::to_vec(&std::collections::BTreeMap::from([("id", vec![entry])])).unwrap(),
        memory,
        &token,
    )
    .unwrap();
    let (acl, mut allowance) = acl.into_parts();
    let (column_acls, columns_memory) = columns.into_parts();
    allowance.absorb(columns_memory);
    Budgeted::new(
        BoundRelationSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl,
            column_acls,
            acl_revisions: RelationAclRevisions::default(),
        }
        .into(),
        allowance,
    )
}

#[test]
fn admitted_relation_restoration_preserves_original_authority_buffers_and_allowance() {
    let (_, roles) = fixture();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let row = admitted(&memory);
    let RelationSecurityRow::Bound(source) = &*row else {
        unreachable!()
    };
    let original_acl = source.acl.as_ref().unwrap().as_ptr();
    let (column, entries) = source.column_acls.iter().next().unwrap();
    let original_column = std::ptr::from_ref(column);
    let original_entries = entries.as_ptr();
    let original_payload = memory.used();
    let expected = restore_security(&row, Some(&["id".into()]), &roles, false).unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let restored =
        restore_security_with_control(row, Some(["id"].into_iter()), &roles, &control).unwrap();
    assert_eq!(*restored, expected);
    assert_eq!(restored.acl.as_ref().unwrap().as_ptr(), original_acl);
    let (column, entries) = restored.column_acls.iter().next().unwrap();
    assert_eq!(std::ptr::from_ref(column), original_column);
    assert_eq!(entries.as_ptr(), original_entries);
    assert_eq!(memory.used(), original_payload);
    token.cancel();
    drop(roles);
    assert_eq!(
        restored.column_acls["id"][0].grantor,
        RoleIdentity::BOOTSTRAP
    );
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn admitted_relation_restoration_preserves_migration_and_authority_diagnostics() {
    let (_, roles) = fixture();
    let token = CancellationToken::new();
    for defect in ["legacy", "revision", "owner", "column", "grantor"] {
        let memory = MemoryBudget::new(1 << 20);
        let row = if defect == "legacy" {
            let owner = ProductionControl::new(&memory, &token, &token)
                .copy_text("uqa")
                .unwrap()
                .into_budgeted()
                .unwrap();
            owner
                .try_map(|owner| Ok::<_, ()>(RelationSecurityRow::legacy(owner)))
                .unwrap()
        } else {
            admitted(&memory)
                .try_map(|row| {
                    let RelationSecurityRow::Bound(mut row) = row else {
                        unreachable!()
                    };
                    match defect {
                        "revision" => {
                            row.acl_revisions.relation = Some([0; 16]);
                            row.role_owner.object_id = [0; 16];
                        }
                        "owner" => row.role_owner.object_id = [0; 16],
                        "grantor" => {
                            row.column_acls.get_mut("id").unwrap()[0].grantor.object_id = [9; 16];
                        }
                        "column" => (),
                        _ => unreachable!(),
                    }
                    Ok::<_, ()>(row.into())
                })
                .unwrap()
        };
        let column = if defect == "column" { "other" } else { "id" };
        let expected = restore_security(&row, Some(&[column.into()]), &roles, false)
            .unwrap_err()
            .to_string();
        let error = restore_security_with_control(
            row,
            Some([column].into_iter()),
            &roles,
            &ProductionControl::new(&memory, &token, &token),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), expected, "{defect}");
        assert_eq!(memory.used(), 0, "{defect}");
    }
}

#[test]
fn admitted_relation_restoration_retires_input_on_resource_failure_and_cancellation() {
    let (_, roles) = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let row = admitted(&memory);
    let held = memory.reserve(memory.available()).unwrap();
    let error = restore_security_with_control(
        row,
        Some(["id"].into_iter()),
        &roles,
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap_err();
    assert_eq!(
        crate::storage_errors::storage_error("restore authority", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), held.bytes());
    drop(held);
    assert_eq!(memory.used(), 0);

    for original in [false, true] {
        for active in [false, true] {
            let row = admitted(&memory);
            let owner = CancellationToken::new();
            let invoking = CancellationToken::new();
            let selected = if original { &owner } else { &invoking };
            if !active {
                selected.cancel();
            }
            let columns = ["id"].into_iter().inspect(|_| {
                if active {
                    selected.cancel();
                }
            });
            assert!(matches!(
                restore_security_with_control(
                    row,
                    Some(columns),
                    &roles,
                    &ProductionControl::new(&memory, &owner, &invoking)
                ),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(memory.used(), 0, "original {original}, active {active}");
        }
    }
    let other = MemoryBudget::new(1 << 20);
    let error = restore_security_with_control(
        admitted(&memory),
        Some(["id"].into_iter()),
        &roles,
        &ProductionControl::new(&other, &token, &token),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "stored relation security requires its original allowance"
    );
    assert_eq!((memory.used(), other.used()), (0, 0));
}
