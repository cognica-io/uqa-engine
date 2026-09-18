//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::roles::{identity::RoleBinding, role_inherits};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn fixture() -> (KeyValueCatalog, BTreeMap<String, RoleDefinition>) {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let initial = super::super::restore_and_migrate(&catalog).unwrap();
    let mut roles = initial.roles.clone();
    for (offset, name) in ["target", "member", "other_target", "other_member"]
        .into_iter()
        .enumerate()
    {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_001 + offset as i64;
        role.object_id = [offset as u8 + 1; 16];
        role.attributes.clear();
        roles.insert(name.into(), role);
    }
    super::super::persist_roles(Some(&catalog), &initial.roles, &roles).unwrap();
    (catalog, roles)
}

fn membership(
    roles: &BTreeMap<String, RoleDefinition>,
    target: &str,
    member: &str,
    oid: i64,
) -> RoleMembership {
    RoleMembership {
        oid,
        role: RoleBinding::from_definition(&roles[target]).unwrap(),
        member: RoleBinding::from_definition(&roles[member]).unwrap(),
        grantor: RoleBinding::from_definition(&roles["uqa"]).unwrap(),
        admin_option: false,
        inherit_option: true,
        set_option: true,
    }
}

#[test]
fn independent_membership_changes_preserve_unrelated_committed_records() {
    let (catalog, roles) = fixture();
    let first = membership(&roles, "target", "member", 30_001);
    let baseline = BTreeMap::from([(first.key(), first.clone())]);
    persist(&catalog, &BTreeMap::new(), &baseline).unwrap();
    let second = membership(&roles, "other_target", "other_member", 30_002);
    let mut peer = baseline.clone();
    peer.insert(second.key(), second.clone());
    persist(&catalog, &baseline, &peer).unwrap();

    let mut private = baseline.clone();
    private.get_mut(&first.key()).unwrap().admin_option = true;
    persist(&catalog, &baseline, &private).unwrap();
    let latest = super::super::restore(&catalog).unwrap();
    assert_eq!(latest.memberships.len(), 2);
    assert!(latest.memberships[&first.key()].admin_option);
    assert_eq!(latest.memberships[&second.key()], second);
    persist(&catalog, &private, &BTreeMap::new()).unwrap();
    let latest = super::super::restore(&catalog).unwrap();
    assert_eq!(latest.memberships, BTreeMap::from([(second.key(), second)]));
    assert!(catalog.get_metadata(&oid_key(first.oid)).unwrap().is_none());
    assert_eq!(
        catalog
            .get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(FORMAT)
    );
}

#[test]
fn membership_oid_collisions_and_invalid_candidates_fail_before_any_write() {
    let (catalog, roles) = fixture();
    let first = membership(&roles, "target", "member", 30_001);
    let second = membership(&roles, "other_target", "other_member", 30_001);
    let baseline = BTreeMap::from([(first.key(), first.clone())]);
    persist(&catalog, &BTreeMap::new(), &baseline).unwrap();
    let stored = catalog.metadata_with_prefix("").unwrap();
    for before in [BTreeMap::new(), baseline.clone()] {
        assert_eq!(
            persist(
                &catalog,
                &before,
                &BTreeMap::from([(second.key(), second.clone())])
            )
            .unwrap_err()
            .sqlstate(),
            Some("23505")
        );
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), stored);
    }
    let mut duplicate = baseline.clone();
    duplicate.insert(second.key(), second.clone());
    assert_eq!(
        persist(&catalog, &baseline, &duplicate)
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    let invalid = BTreeMap::from([(first.key(), second)]);
    assert!(persist(&catalog, &baseline, &invalid).is_err());
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), stored);
}

#[test]
fn membership_records_reject_malformed_markers_keys_values_and_oid_claims() {
    for corruption in 0..9 {
        let (catalog, roles) = fixture();
        let value = membership(&roles, "target", "member", 30_001);
        persist(
            &catalog,
            &BTreeMap::new(),
            &BTreeMap::from([(value.key(), value.clone())]),
        )
        .unwrap();
        match corruption {
            0 => catalog.set_metadata(
                ROLE_MEMBERSHIPS_METADATA_KEY,
                r#"{"role_membership_catalog_format":2}"#,
            ),
            1 => catalog.delete_metadata(ROLE_MEMBERSHIPS_METADATA_KEY),
            2 => catalog.set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, "[]"),
            3 => catalog.delete_metadata(&oid_key(value.oid)),
            4 => catalog.set_metadata(&oid_key(value.oid), "absent"),
            5 => catalog.set_metadata(&oid_key(value.oid + 1), &key(&value.key())),
            6 => catalog.set_metadata(
                &format!("{PREFIX}invalid"),
                &serde_json::to_string(&value).unwrap(),
            ),
            7 => catalog.set_metadata(&key(&value.key()), "{"),
            8 => catalog.delete_metadata(&key(&value.key())),
            _ => unreachable!(),
        }
        .unwrap();
        assert!(
            super::super::restore(&catalog).is_err(),
            "corruption {corruption}"
        );
    }
}

#[test]
fn legacy_memberships_require_initial_open_and_keep_their_public_oid() {
    let (catalog, roles) = fixture();
    let legacy = r#"[{"oid":30001,"role":"target","member":"member","grantor":"uqa","admin_option":true,"inherit_option":false,"set_option":true}]"#;
    catalog
        .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, legacy)
        .unwrap();
    assert!(super::super::restore(&catalog)
        .err()
        .unwrap()
        .to_string()
        .contains("initial-open record migration"));
    assert_eq!(
        catalog
            .get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(legacy)
    );
    let restored = super::super::restore_and_migrate(&catalog).unwrap();
    let mut expected = membership(&roles, "target", "member", 30_001);
    expected.admin_option = true;
    expected.inherit_option = false;
    assert_eq!(
        restored.memberships,
        BTreeMap::from([(expected.key(), expected)])
    );
    assert_eq!(
        restored.memberships,
        super::super::restore(&catalog).unwrap().memberships
    );
}

#[test]
fn membership_reopen_retains_dropped_endpoints_without_authorizing_their_replacements() {
    for endpoint in ["target", "member"] {
        let (catalog, roles) = fixture();
        let value = membership(&roles, "target", "member", 30_001);
        let baseline = BTreeMap::from([(value.key(), value)]);
        persist(&catalog, &BTreeMap::new(), &baseline).unwrap();
        let mut dropped = roles.clone();
        dropped.remove(endpoint);
        super::super::persist_roles(Some(&catalog), &roles, &dropped).unwrap();
        assert_eq!(
            super::super::restore(&catalog).unwrap().memberships,
            baseline
        );
        let mut replaced = dropped.clone();
        let mut replacement = roles[endpoint].clone();
        replacement.object_id = [5; 16];
        replaced.insert(endpoint.into(), replacement);
        super::super::persist_roles(Some(&catalog), &dropped, &replaced).unwrap();
        let restored = super::super::restore(&catalog).unwrap();
        assert_eq!(restored.memberships, baseline);
        assert!(!role_inherits(
            &restored.roles,
            &restored.memberships,
            "member",
            "target"
        ));
    }
}

#[test]
fn invalid_legacy_membership_never_leaves_partial_record_conversion() {
    let (catalog, _) = fixture();
    let legacy = r#"[{"oid":30001,"role":"target","member":"member","grantor":"missing","admin_option":true,"inherit_option":false,"set_option":true}]"#;
    catalog
        .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, legacy)
        .unwrap();
    let stored = catalog.metadata_with_prefix("").unwrap();
    assert!(super::super::restore_and_migrate(&catalog).is_err());
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), stored);
}
