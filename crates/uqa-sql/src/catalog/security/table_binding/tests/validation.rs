//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{roles, security};
use crate::catalog::{roles::RoleDefinitions, security::BoundTableSecurity};
use crate::SQLError;
use uqa_core::{
    catalog_acl::TablePrivileges,
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn validate(
    value: &BoundTableSecurity,
    roles: &RoleDefinitions,
    columns: Option<&[&str]>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    value.validate_with_control(columns.map(|names| names.iter().copied()), roles, control)
}

#[test]
fn admitted_table_authority_preserves_original_diagnostics_and_validation_order() {
    let roles = roles();
    let source = security(&roles);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in 0..10 {
        let mut value = source.clone();
        let expected = match case {
            0 => {
                value.acl_revisions.relation = Some([0; 16]);
                value.role_owner.object_id = [0; 16];
                "invalid relation or attribute ACL tuple identity"
            }
            1 => {
                value
                    .acl_revisions
                    .columns
                    .insert("missing".into(), [1; 16]);
                value.role_owner.object_id = [0; 16];
                "invalid relation or attribute ACL tuple identity"
            }
            2 => {
                value.role_owner.object_id = [0; 16];
                "invalid persisted table role identity"
            }
            3 => {
                let first = value.acl.as_ref().unwrap()[1].clone();
                value.acl.as_mut().unwrap().push(first);
                "ACL contains duplicate grant path `owner` -> `grantor`"
            }
            4 => {
                let entry = &mut value.acl.as_mut().unwrap()[1];
                entry.privileges = TablePrivileges::default();
                entry.grant_options = TablePrivileges::default();
                "ACL contains an empty grant path"
            }
            5 => {
                value.acl.as_mut().unwrap()[1].role = None;
                "PUBLIC cannot hold grant options"
            }
            6 => {
                value.acl.as_mut().unwrap()[1].privileges.select = false;
                "ACL grant option exists without its privilege"
            }
            7 => {
                value.acl.as_mut().unwrap()[1].grantor = roles["reader"].identity();
                "ACL grant path from `reader` is not rooted at owner `owner`"
            }
            8 => {
                value.column_acls.get_mut("id").unwrap()[0]
                    .privileges
                    .delete = true;
                "column ACL for `id` contains a relation-only privilege"
            }
            _ => {
                value.acl.as_mut().unwrap()[1].privileges = TablePrivileges::default();
                // Complete endpoint resolution precedes validation of the malformed table ACL.
                value.column_acls.get_mut("id").unwrap()[0]
                    .role
                    .as_mut()
                    .unwrap()
                    .object_id = [0; 16];
                "invalid persisted table role identity"
            }
        };
        assert!(
            matches!(validate(&value, &roles, Some(&["id"]), &control), Err(SQLError::Internal(ref message)) if message == expected),
            "case {case}"
        );
        assert_eq!(
            value.validate(Some(&["id".into()]), &roles).unwrap_err(),
            expected
        );
        assert_eq!(memory.used(), 0, "case {case}");
    }
    for (columns, expected) in [
        (None, "column ACLs require durable public column metadata"),
        (
            Some(["other"].as_slice()),
            "column ACL references missing column `id`",
        ),
    ] {
        assert!(
            matches!(validate(&source, &roles, columns, &control), Err(SQLError::Internal(ref message)) if message == expected)
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_table_authority_releases_projection_and_grant_scratch_on_every_exit() {
    let roles = roles();
    let source = security(&roles);
    let original_source = source.clone();
    let token = CancellationToken::new();
    let mut successes = 0;
    let mut failures = 0;
    for limit in [0, 8, 64, 256, 512, 1024, 4096, 65536] {
        let memory = MemoryBudget::new(limit);
        let control = ProductionControl::new(&memory, &token, &token);
        match validate(&source, &roles, Some(&["id"]), &control) {
            Ok(()) => successes += 1,
            Err(ref error) if error.sqlstate() == Some("53200") => failures += 1,
            result => panic!("unexpected validation result at {limit}: {result:?}"),
        }
        assert_eq!(memory.used(), 0, "limit {limit}");
    }
    assert!(successes > 0 && failures > 0);
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert!(matches!(
            validate(&source, &roles, Some(&["id"]), &control),
            Err(SQLError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
    assert!(source.acl_revisions.columns.is_empty());
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let cancellation = if cancel_original {
            &original
        } else {
            &invoking
        };
        let observed = std::cell::Cell::new(false);
        let columns = ["id"].into_iter().inspect(|_| {
            assert!(memory.used() > 0, "resolved authority must remain admitted");
            observed.set(true);
            cancellation.cancel();
        });
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert!(matches!(
            source.validate_with_control(Some(columns), &roles, &control),
            Err(SQLError::Cancelled(_))
        ));
        assert!(observed.get(), "validation must reach the borrowed column");
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(source, original_source);
}

#[test]
fn admitted_table_authority_rejects_large_name_projection_before_copying() {
    let mut roles = roles();
    let source = security(&roles);
    let mut owner = roles["owner"].clone();
    roles.remove("owner").unwrap();
    owner.name = "x".repeat(2 << 20);
    roles.insert(owner.name.clone(), owner).unwrap();
    let memory = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            validate(&source, &roles, Some(&["id"]), &control),
            Err(ref error) if error.sqlstate() == Some("53200")
        ));
    });
    assert!(measured.bytes_total < 8192, "{measured:?}");
    assert_eq!(memory.used(), 0);
}

#[test]
fn admitted_table_authority_uses_current_names_and_separate_relation_column_closures() {
    let mut roles = roles();
    let mut source = security(&roles);
    let grantor = roles["grantor"].identity();
    let reader = roles["reader"].identity();
    for name in ["owner", "grantor", "reader"] {
        let mut role = roles[name].clone();
        roles.remove(name).unwrap();
        role.name = format!("renamed {name}");
        roles.insert(role.name.clone(), role).unwrap();
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    validate(&source, &roles, Some(&["id"]), &control).unwrap();
    // A column grant cannot make its recipient a relation grantor. This cycle has no owner-rooted table path.
    source.acl.as_mut().unwrap()[1].grantor = reader;
    let column = &mut source.column_acls.get_mut("id").unwrap()[0];
    column.role = Some(reader);
    column.grantor = grantor;
    column.grant_options.select = true;
    assert!(
        matches!(validate(&source, &roles, Some(&["id"]), &control), Err(SQLError::Internal(ref message)) if message == "ACL grant path from `renamed reader` is not rooted at owner `renamed owner`")
    );
    assert_eq!(memory.used(), 0);
}
