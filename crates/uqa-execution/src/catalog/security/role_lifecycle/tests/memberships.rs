//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{grant_roles, Catalog, GrantRoleStmt, RoleAttribute, RoleMembershipOptions};
use crate::{
    catalog::security::{role_lifecycle::identity, roles::locking::ROLE_CATALOG_CLASS_ID},
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use std::collections::{BTreeMap, BTreeSet};

fn catalog() -> Catalog {
    let catalog = Catalog::new();
    for name in ["target", "member", "grantor"] {
        catalog.role(name, &[RoleAttribute::Inherit]);
    }
    catalog.membership("target", "grantor", "uqa");
    *catalog.current.borrow_mut() = "grantor".into();
    catalog
}

fn statement() -> GrantRoleStmt {
    GrantRoleStmt {
        granted_roles: vec!["target".into()],
        grantee_roles: vec!["member".into()],
        is_grant: true,
        options: RoleMembershipOptions::default(),
        grantor: None,
        cascade: false,
    }
}

#[test]
fn grant_captures_authority_before_wait_and_locks_only_target_oid_and_new_grantor() {
    let catalog = catalog();
    let roles = catalog.roles.borrow().clone();
    catalog
        .refreshed_memberships
        .borrow_mut()
        .push_back(BTreeMap::new());
    let mut fresh = roles.clone();
    fresh.remove("target");
    catalog.refreshed_roles.borrow_mut().push_back(fresh);
    grant_roles(&catalog.context(), &statement()).unwrap();
    let memberships = catalog.memberships.borrow();
    let row = memberships.values().next().unwrap();
    assert_eq!(memberships.len(), 1);
    assert_eq!(row.role.identity(), roles["target"].identity());
    assert_eq!(row.grantor.identity(), roles["grantor"].identity());
    assert_eq!(
        *catalog.catalog_locks.borrow(),
        [
            (
                ROLE_CATALOG_CLASS_ID,
                Some(roles["target"].oid as u32),
                RelationLockMode::ShareUpdateExclusive
            ),
            (
                identity::MEMBERSHIP_CATALOG_CLASS_ID,
                Some(row.oid as u32),
                RelationLockMode::AccessExclusive
            ),
            (
                ROLE_CATALOG_CLASS_ID,
                Some(roles["grantor"].oid as u32),
                RelationLockMode::AccessShare
            ),
        ]
    );
    let events = catalog.events.borrow();
    let writer = events.iter().position(|event| event == "writer").unwrap();
    assert!(events.iter().rposition(|event| event == "refresh").unwrap() < writer);
    assert!(!events
        .iter()
        .any(|event| event == "persist roles" || event == "publish roles"));
}

#[test]
fn new_member_inherit_reads_current_attributes_but_does_not_rebind_replacements() {
    for change in ["attributes", "dropped", "replaced", "renamed"] {
        for explicit in [false, true] {
            let catalog = catalog();
            let mut roles = catalog.roles.borrow().clone();
            let original = roles["member"].identity();
            let mut member = roles.remove("member").unwrap();
            match change {
                "attributes" => {
                    member.attributes.remove(&RoleAttribute::Inherit);
                }
                "replaced" => member.object_id = [99; 16],
                "renamed" => member.name = "renamed".into(),
                "dropped" => {}
                _ => unreachable!(),
            }
            if change != "dropped" {
                roles.insert(member.name.clone(), member);
            }
            catalog.refreshed_roles.borrow_mut().push_back(roles);
            let mut statement = statement();
            statement.options.inherit = explicit.then_some(true);
            let result = grant_roles(&catalog.context(), &statement);
            if !explicit && matches!(change, "dropped" | "replaced") {
                assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
                assert_eq!(catalog.epoch.get(), 0);
            } else {
                result.unwrap();
                let memberships = catalog.memberships.borrow();
                let row = memberships
                    .values()
                    .find(|row| row.member.identity() == original)
                    .unwrap();
                assert_eq!(row.inherit_option, explicit || change != "attributes");
            }
            catalog.released();
        }
    }
}

#[test]
fn new_grantor_dependency_rejects_original_incarnation_removed_during_wait() {
    for replace in [false, true] {
        let catalog = catalog();
        let mut roles = catalog.roles.borrow().clone();
        let grantor = roles.remove("grantor").unwrap();
        if replace {
            let mut replacement = grantor.clone();
            replacement.object_id = [99; 16];
            roles.insert(replacement.name.clone(), replacement);
        }
        catalog.refreshed_roles.borrow_mut().push_back(roles);
        let before = catalog.memberships.borrow().clone();
        let error = grant_roles(&catalog.context(), &statement()).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        assert!(error
            .to_string()
            .contains(&format!("role {} was concurrently dropped", grantor.oid)));
        assert_eq!(*catalog.memberships.borrow(), before);
        assert_eq!(catalog.epoch.get(), 0);
        assert!(!catalog
            .events
            .borrow()
            .iter()
            .any(|event| event == "writer"));
        catalog.released();
    }
}

#[test]
fn membership_oid_retries_committed_and_staged_collisions_and_releases_losing_lock() {
    let catalog = catalog();
    let mut fresh = catalog.memberships.borrow().clone();
    fresh.values_mut().next().unwrap().oid = 36_000;
    catalog.refreshed_memberships.borrow_mut().push_back(fresh);
    let mut candidates = [36_002, 36_000, 36_001].into_iter();
    let oid =
        identity::reserve_membership_oid(&catalog.context(), &BTreeSet::from([36_002]), || {
            Ok(candidates.next().unwrap())
        })
        .unwrap();
    assert_eq!(oid, 36_001);
    assert_eq!(catalog.catalog_locks.borrow().len(), 2);
    for (oid, available) in [(36_000, true), (36_001, false), (36_002, true)] {
        let key = catalog.locks.shared_catalog_key(SharedCatalogLock::Object {
            class_id: identity::MEMBERSHIP_CATALOG_CLASS_ID,
            oid,
        });
        assert_eq!(
            catalog
                .locks
                .try_acquire_relation(
                    2,
                    key,
                    RelationLockMode::AccessExclusive,
                    0,
                    &catalog.cancel
                )
                .unwrap(),
            available
        );
    }
    catalog.released();
}

#[test]
fn unchanged_grant_and_missing_revoke_emit_notices_after_guards_without_allocating_oids() {
    let catalog = catalog();
    catalog.membership("target", "member", "grantor");
    let before = catalog.memberships.borrow().clone();
    grant_roles(&catalog.context(), &statement()).unwrap();
    let events = catalog.events.borrow();
    assert!(events
        .iter()
        .any(|event| event.starts_with("NOTICE: role \"member\" has already been granted")));
    assert!(!events
        .iter()
        .any(|event| event == "writer" || event.starts_with("publish")));
    drop(events);
    assert_eq!(*catalog.memberships.borrow(), before);
    catalog.events.borrow_mut().clear();
    catalog
        .memberships
        .borrow_mut()
        .retain(|_, row| row.member.name != "member");
    let mut statement = statement();
    statement.is_grant = false;
    grant_roles(&catalog.context(), &statement).unwrap();
    assert!(catalog
        .events
        .borrow()
        .iter()
        .any(|event| event.starts_with("WARNING: role \"member\" has not been granted")));
    assert!(catalog
        .catalog_locks
        .borrow()
        .iter()
        .all(|(class, _, mode)| *class == ROLE_CATALOG_CLASS_ID
            && *mode == RelationLockMode::ShareUpdateExclusive));
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn cycle_validation_reads_the_graph_after_the_target_wait() {
    let catalog = catalog();
    let before = catalog.memberships.borrow().clone();
    catalog.membership("member", "target", "uqa");
    let after = catalog.memberships.replace(before);
    catalog
        .refreshed_memberships
        .borrow_mut()
        .push_back(after.clone());
    let error = grant_roles(&catalog.context(), &statement()).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0LP01"));
    assert_eq!(*catalog.memberships.borrow(), after);
    assert_eq!(catalog.catalog_locks.borrow().len(), 1);
    assert!(!catalog
        .events
        .borrow()
        .iter()
        .any(|event| event == "writer"));
    assert_eq!(catalog.epoch.get(), 0);
}

#[test]
fn writer_refresh_cannot_resurrect_or_overwrite_a_concurrently_changed_membership() {
    for remove in [false, true] {
        let catalog = catalog();
        catalog.membership("target", "member", "grantor");
        let mut peer = catalog.memberships.borrow().clone();
        let key = peer
            .values()
            .find(|row| row.member.name == "member")
            .unwrap()
            .key();
        if remove {
            peer.remove(&key);
        } else {
            peer.get_mut(&key).unwrap().inherit_option = true;
        }
        *catalog.writer_memberships.borrow_mut() = Some(peer.clone());
        let mut statement = statement();
        statement.options.admin = Some(false);
        let error = grant_roles(&catalog.context(), &statement).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(*catalog.memberships.borrow(), peer);
        assert!(!catalog
            .events
            .borrow()
            .iter()
            .any(|event| event.starts_with("persist") || event.starts_with("publish")));
        assert_eq!(catalog.epoch.get(), 0);
    }
}
