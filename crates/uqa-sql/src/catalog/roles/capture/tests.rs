//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::identity::{RoleBinding, RoleReference};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn membership(incarnation: u8, grantor: u8) -> RoleMembership {
    RoleMembership {
        oid: 20_000 + i64::from(grantor),
        role: RoleBinding::new("Renamed.Team".into(), 17_001, [incarnation; 16]),
        member: RoleBinding::new("사용자".into(), 17_002, [3; 16]),
        grantor: RoleBinding::new("grantor".into(), 17_003, [grantor; 16]),
        admin_option: incarnation == 1,
        inherit_option: grantor == 4,
        set_option: incarnation == 2,
    }
}

#[test]
fn role_capture_preserves_incarnations_revisions_attributes_and_exact_names() {
    let mut role = RoleDefinition::bootstrap();
    role.oid = 42_001;
    role.object_id = [19; 16];
    role.revision = u64::MAX;
    role.name = "Exact.Name 사용자".into();
    role.attributes.insert(RoleAttribute::Replication);
    role.connection_limit = 17;
    let source = BTreeMap::from([(role.name.clone(), role)]);
    let memory = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let captured = roles(&source, &control).unwrap().into_budgeted().unwrap();
    drop(source);
    assert_eq!(captured[0].0, "Exact.Name 사용자");
    let saved = &captured[0].1;
    assert_eq!(saved.name, captured[0].0);
    assert_eq!(saved.oid, 42_001);
    assert_eq!(saved.object_id, [19; 16]);
    assert_eq!(saved.revision, u64::MAX);
    assert_eq!(saved.connection_limit, 17);
    assert_eq!(
        saved.attributes,
        [
            RoleAttribute::Superuser,
            RoleAttribute::Inherit,
            RoleAttribute::CreateRole,
            RoleAttribute::CreateDb,
            RoleAttribute::Login,
            RoleAttribute::Replication,
            RoleAttribute::BypassRls,
        ]
    );
    assert!(memory.used() >= 2 * saved.name.len() + size_of_val(saved));
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn membership_capture_keeps_distinct_incarnations_grant_paths_and_all_options() {
    let source = [membership(1, 4), membership(1, 5), membership(2, 4)]
        .into_iter()
        .map(|entry| (entry.key(), entry))
        .collect::<BTreeMap<_, _>>();
    let memory = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let captured = memberships(&source, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    assert_eq!(captured.len(), 3);
    assert!(captured.iter().map(|(k, v)| (k, v)).eq(source.iter()));
    drop(source);
    assert_eq!(captured[0].1.role.name, "Renamed.Team");
    assert_eq!(captured[0].1.member.name, "사용자");
    assert!(memory.used() >= 3 * size_of::<(RoleMembershipKey, RoleMembership)>());
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn role_capture_uses_independent_admission_and_retires_failed_production() {
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let mut role = RoleDefinition::bootstrap();
    role.name = "x".repeat(32_768);
    let source = BTreeMap::from([("key".into(), role)]);
    assert_eq!(
        roles(&source, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    let mut member = membership(1, 4);
    member.grantor.name = "g".repeat(32_768);
    assert_eq!(
        member.capture_membership(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);

    let source_memory = MemoryBudget::new(4096);
    let source_cancel = CancellationToken::new();
    let source_control = ProductionControl::new(&source_memory, &source_cancel, &source_cancel);
    let RoleReference::Bound(binding) =
        RoleReference::from_definition_with_control(&RoleDefinition::bootstrap(), &source_control)
            .unwrap()
    else {
        panic!("role selection must retain a bound incarnation");
    };
    assert!(source_memory.used() > 0);
    let saved = binding
        .capture_binding(&control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    source_cancel.cancel();
    drop(binding);
    assert_eq!(source_memory.used(), 0);
    assert_eq!(saved.name, "uqa");
    assert_eq!(saved.identity(), super::super::RoleIdentity::BOOTSTRAP);
    assert!(memory.used() >= saved.name.len());
    drop(saved);
    assert_eq!(memory.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        roles(&source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        member.capture_membership(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
