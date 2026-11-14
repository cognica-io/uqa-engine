//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::roles::persistence::{
    persist_memberships, persist_roles, restore, restore_and_migrate,
};
use uqa_sql::catalog::roles::identity::RoleBinding;

const LIMITS: CatalogTextLimits = CatalogTextLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 65536,
};

fn fixture(count: usize) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let initial = restore_and_migrate(&*catalog).unwrap();
    let mut roles = initial.roles.clone();
    let mut memberships = initial.memberships.clone();
    for index in 0..count {
        let mut role = RoleDefinition::bootstrap();
        role.oid = 40000 + i64::try_from(index).unwrap();
        role.name = format!("member_{index:04}_역할");
        role.object_id = (u128::try_from(index).unwrap() + 1).to_le_bytes();
        let membership = RoleMembership {
            oid: 50000 + i64::try_from(index).unwrap(),
            role: RoleBinding::from_definition(&initial.roles["uqa"]).unwrap(),
            member: RoleBinding::from_definition(&role).unwrap(),
            grantor: RoleBinding::from_definition(&initial.roles["uqa"]).unwrap(),
            admin_option: index % 2 == 0,
            inherit_option: true,
            set_option: false,
        };
        roles.insert(role.name.clone(), role).unwrap();
        memberships.insert(membership.key(), membership).unwrap();
    }
    persist_roles(Some(&*catalog), &initial.roles, &roles).unwrap();
    persist_memberships(Some(&*catalog), &initial.memberships, &memberships).unwrap();
    catalog
}

fn finish(input: &mut RoleCatalogInput) -> StorageBackendResult<(RoleCatalogValues, usize)> {
    for step in 1..32768 {
        if let Some(values) = input.advance(1)? {
            return Ok((values, step));
        }
    }
    panic!("role input stopped progressing");
}

#[test]
fn persistent_role_input_matches_current_catalog_and_bounds_native_population_work() {
    for count in [0, 8, 128] {
        let catalog = fixture(count);
        let expected = restore(&*catalog).unwrap();
        catalog
            .set_metadata("unrelated_large_metadata", &"x".repeat(1 << 18))
            .unwrap();
        let before = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let mut input =
            RoleCatalogInput::new(Arc::clone(&catalog), LIMITS, &mutations, &control).unwrap();
        let (mut values, steps) = finish(&mut input).unwrap();
        assert_eq!(values.roles, expected.roles);
        assert_eq!(values.memberships, expected.memberships);
        assert!(steps > before.len());
        assert!(steps <= before.len() * 5 + (count + 1) * 8 + 64, "{steps}");
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
        assert!(input.advance(1).is_err());
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        values
            .roles
            .edit("uqa", |role| {
                role.connection_limit = 5;
                Ok(())
            })
            .unwrap();
        assert_eq!(values.roles["uqa"].connection_limit, 5);
        drop((values, input));
        assert_eq!(mutations.used(), 0);
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_role_input_preserves_definition_claim_identity_and_membership_error_order() {
    let catalog = fixture(1);
    let name = "member_0000_역할";
    let original = restore(&*catalog).unwrap();
    let key = records::role_key(name);
    let mut role = original.roles[name].clone();
    role.name = "wrong".into();
    role.object_id = [0; 16];
    catalog
        .set_metadata(&key, &serde_json::to_string(&role).unwrap())
        .unwrap();
    catalog
        .set_metadata("uqa.sql.role_oid.v1:40000", "absent")
        .unwrap();
    catalog
        .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, "{")
        .unwrap();
    for (stage, expected) in [
        (
            0,
            "persisted role key `member_0000_역할` does not match role name `wrong`",
        ),
        (1, "role OID records do not match role definitions"),
        (
            2,
            "persisted role `member_0000_역할` has no object identity",
        ),
        (3, "persisted role `member_0000_역할` has no tuple revision"),
    ] {
        if stage == 1 {
            role.name = name.into();
        }
        if stage == 2 {
            catalog
                .set_metadata("uqa.sql.role_oid.v1:40000", name)
                .unwrap();
        }
        if stage == 3 {
            role.object_id = [1; 16];
            role.revision = 0;
        }
        catalog
            .set_metadata(&key, &serde_json::to_string(&role).unwrap())
            .unwrap();
        let before = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = RoleCatalogInput::new(
            Arc::clone(&catalog),
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        assert_eq!(finish(&mut input).err().unwrap().to_string(), expected);
        assert_eq!(restore(&*catalog).err().unwrap().to_string(), expected);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    }
}

#[test]
fn persistent_role_input_rejects_noncurrent_markers_without_performing_migration() {
    for version in [1, 2, 4] {
        let catalog = fixture(0);
        catalog
            .set_metadata(
                ROLES_METADATA_KEY,
                &format!(r#"{{"role_catalog_format":{version}}}"#),
            )
            .unwrap();
        let before = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = RoleCatalogInput::new(
            Arc::clone(&catalog),
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        let error = finish(&mut input).err().unwrap().to_string();
        assert_eq!(
            error,
            if version == 4 {
                "unsupported role catalog format 4"
            } else {
                "role metadata requires initial-open record migration"
            }
        );
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_role_input_failure_retires_original_rows_private_roots_and_continuation() {
    for failure in ["cancel", "memory", "zero", "drop"] {
        let catalog = fixture(32);
        let before = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let mut input = RoleCatalogInput::new(
            Arc::clone(&catalog),
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        for _ in 0..48 {
            assert!(input.advance(1).unwrap().is_none());
        }
        assert!(control.memory().used() > 0);
        let occupied = (failure == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if failure == "cancel" {
            control.cancellation().cancel();
        }
        if failure != "drop" {
            assert!(input.advance(usize::from(failure != "zero")).is_err());
            assert!(input.advance(1).is_err());
        }
        drop(input);
        assert_eq!(
            control.memory().used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    }
}

#[test]
fn persistent_role_input_validates_all_membership_record_fields_before_grantor_resolution() {
    let catalog = fixture(2);
    let rows = catalog.metadata_with_prefix(memberships::PREFIX).unwrap();
    let mut first: RoleMembership = serde_json::from_str(&rows[0].1).unwrap();
    first.grantor = RoleBinding::new("absent".into(), 60000, [9; 16]);
    catalog.delete_metadata(&rows[0].0).unwrap();
    catalog
        .set_metadata(
            &memberships::key(&first.key()),
            &serde_json::to_string(&first).unwrap(),
        )
        .unwrap();
    catalog.set_metadata(&rows[1].0, "{").unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = RoleCatalogInput::new(
        Arc::clone(&catalog),
        LIMITS,
        &MemoryBudget::new(1 << 20),
        &control,
    )
    .unwrap();
    assert!(!finish(&mut input)
        .err()
        .unwrap()
        .to_string()
        .contains("grantor identity"));
    assert!(!restore(&*catalog)
        .err()
        .unwrap()
        .to_string()
        .contains("grantor identity"));
    assert_eq!(control.memory().used(), 0);
}
