//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identity, security, rollback and rejected-batch sequence cases.

use super::*;

pub(super) fn reject<T, E>(
    connection: &ManagedConnection,
    native: bool,
    operation: impl FnOnce() -> Result<T, E>,
) {
    connection.savepoint("recover_error").unwrap();
    assert!(operation().is_err());
    if !native {
        // Legacy physical SQL marks its transaction aborted; native evaluation discards its local batch.
        connection.rollback_to_savepoint("recover_error").unwrap();
    }
    connection.release_savepoint("recover_error").unwrap();
}

#[test]
fn sequence_catalog_lifecycle_preserves_security_and_names_in_legacy_and_native_sessions() {
    for native in [false, true] {
        for dependency in [
            SequenceOwnerDependency::Automatic,
            SequenceOwnerDependency::Internal,
        ] {
            let (connection, catalog) = memory(native);
            catalog.save_schema("archive").unwrap();
            let mut row = sequence("z.dot\" name", 1);
            row.persistence = "u".into();
            row.owner = Some(SequenceOwner {
                table_object_id: [20; 16],
                column_object_id: [21; 16],
                dependency,
            });
            row.security = SequenceSecurityRow::Bound(BoundSequenceSecurity {
                role_owner: RoleIdentity::BOOTSTRAP,
                acl: Some(vec![BoundAclEntry {
                    role: Some(RoleIdentity {
                        oid: 20_001,
                        object_id: [1; 16],
                    }),
                    grantor: RoleIdentity::BOOTSTRAP,
                    privileges: SequencePrivileges::ALL,
                    grant_options: SequencePrivileges {
                        usage: true,
                        ..SequencePrivileges::default()
                    },
                }]),
            });
            row.increment = -2;
            row.start = i64::MIN + 2;
            row.current = row.start;
            row.options.min_value = None;
            row.options.max_value = None;
            assert!(catalog.create_sequence_row(&row).unwrap());
            row.options.min_value = Some(i64::MIN);
            row.options.max_value = Some(-1);
            assert_eq!(
                catalog.load_sequence_rows().unwrap(),
                std::slice::from_ref(&row)
            );
            assert!(!catalog.create_sequence_row(&row).unwrap());
            let from = row.relation.qualified_name();
            assert!(catalog.rename_sequence_row(&from, &from).unwrap());
            connection.begin_transaction().unwrap();
            connection.savepoint("keep").unwrap();
            row.relation = RelationIdentity::new("archive", "quoted.name");
            let to = row.relation.qualified_name();
            assert!(catalog.rename_sequence_row(&from, &to).unwrap());
            assert_eq!(
                catalog.load_sequence_rows().unwrap(),
                std::slice::from_ref(&row)
            );
            assert!(catalog.drop_sequence_row(&to).unwrap());
            assert!(catalog.load_sequence_rows().unwrap().is_empty());
            connection.rollback_to_savepoint("keep").unwrap();
            assert_eq!(
                catalog.load_sequence_rows().unwrap()[0]
                    .relation
                    .qualified_name(),
                from
            );
            assert!(catalog.rename_sequence_row(&from, &to).unwrap());
            row.security = SequenceSecurityRow::Bound(BoundSequenceSecurity {
                role_owner: RoleIdentity {
                    oid: 20_002,
                    object_id: [2; 16],
                },
                acl: Some(vec![]),
            });
            assert!(catalog.replace_sequence_row(&row).unwrap());
            connection.commit_transaction().unwrap();
            assert_eq!(
                catalog.load_sequence_rows().unwrap(),
                std::slice::from_ref(&row)
            );
            assert!(catalog.create_sequence_row(&sequence("a", 2)).unwrap());
            assert_eq!(
                catalog
                    .load_sequence_rows()
                    .unwrap()
                    .iter()
                    .map(|row| row.relation.schema.as_str())
                    .collect::<Vec<_>>(),
                ["archive", "public"]
            );
            assert!(catalog.drop_sequence_row(&to).unwrap());
            catalog.drop_schema("archive").unwrap();
            let replacement = sequence("z.dot\" name", 3);
            assert!(catalog.create_sequence_row(&replacement).unwrap());
            assert_eq!(
                catalog.load_sequence_rows().unwrap(),
                [sequence("a", 2), replacement]
            );
        }
    }
}

#[test]
fn sequence_catalog_rejects_name_and_constraint_errors_without_partial_changes() {
    for native in [false, true] {
        let (connection, catalog) = memory(native);
        catalog.save_table(&schema("taken", 30, 30)).unwrap();
        let original = sequence("s", 1);
        catalog.create_sequence_row(&original).unwrap();
        connection.begin_transaction().unwrap();
        assert!(!catalog.rename_sequence_row("absent", "s").unwrap());
        assert!(!catalog.drop_sequence_row("absent").unwrap());
        assert!(!catalog
            .replace_sequence_row(&sequence("absent", 2))
            .unwrap());
        reject(&connection, native, || {
            catalog.create_sequence_row(&sequence("taken", 2))
        });
        reject(&connection, native, || {
            catalog.rename_sequence_row("s", "taken")
        });
        reject(&connection, native, || {
            catalog.rename_sequence_row("s", "missing.s")
        });
        for invalid in ["persistence", "cache", "log"] {
            let mut row = sequence("rejected", 3);
            match invalid {
                "persistence" => row.persistence = "t".into(),
                "cache" => row.options.cache_size = 0,
                _ => row.log_count = -1,
            }
            reject(&connection, native, || catalog.create_sequence_row(&row));
            row.relation = original.relation.clone();
            reject(&connection, native, || catalog.replace_sequence_row(&row));
            assert_eq!(
                catalog.load_sequence_rows().unwrap(),
                std::slice::from_ref(&original)
            );
        }
        reject(&connection, native, || {
            catalog.set_sequence_value(
                "s",
                original.object_id,
                original.definition_generation,
                10,
                true,
                -1,
            )
        });
        assert_eq!(
            catalog.load_sequence_rows().unwrap(),
            std::slice::from_ref(&original)
        );
        assert_eq!(
            catalog
                .set_sequence_value(
                    "missing",
                    original.object_id,
                    original.definition_generation,
                    10,
                    true,
                    -1
                )
                .unwrap(),
            uqa_storage::SequenceSetValueResult::Missing
        );
        let rejected = sequence("rejected", 3);
        assert!(catalog.create_sequence_row(&rejected).unwrap());
        reject(&connection, native, || {
            catalog.rename_sequence_row("s", "rejected")
        });
        connection.commit_transaction().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap(), [rejected, original]);
    }
}

#[test]
fn native_sequence_identity_changes_preserve_retained_generations_and_reject_aliases() {
    let (connection, catalog) = memory(true);
    let mut row = sequence("s", 0);
    row.definition_generation = [0; 16];
    assert!(catalog.create_sequence_row(&row).unwrap());
    let assigned = catalog.load_sequence_rows().unwrap().remove(0);
    assert_ne!(assigned.object_id, [0; 16]);
    assert_ne!(assigned.definition_generation, [0; 16]);
    assert!(catalog.replace_sequence_row(&row).unwrap());
    assert_eq!(
        catalog.load_sequence_rows().unwrap(),
        std::slice::from_ref(&assigned)
    );
    let other = connection.new_session();
    let reader = Catalog::open(other.clone()).unwrap();
    other.begin_transaction().unwrap();
    assert_eq!(
        reader.load_sequence_rows().unwrap(),
        std::slice::from_ref(&assigned)
    );
    let mut alias = assigned.clone();
    alias.relation.name = "alias".into();
    assert!(catalog.create_sequence_row(&alias).is_err());
    alias.definition_generation = [90; 16];
    assert!(catalog.create_sequence_row(&alias).is_err());
    assert_eq!(
        catalog.load_sequence_rows().unwrap(),
        std::slice::from_ref(&assigned)
    );
    connection.begin_transaction().unwrap();
    let mut next = assigned.clone();
    next.definition_generation = [91; 16];
    next.current = 100;
    assert!(catalog.replace_sequence_row(&next).unwrap());
    assert_eq!(
        catalog
            .reserve_sequence_values("s", assigned.object_id, assigned.definition_generation)
            .unwrap(),
        SequenceReservationResult::DefinitionChanged
    );
    assert!(catalog.rename_sequence_row("s", "renamed").unwrap());
    next.relation.name = "renamed".into();
    assert_eq!(reserve(&catalog, &next).first_value, 100);
    let recreated = sequence("s", 3);
    assert!(catalog.create_sequence_row(&recreated).unwrap());
    connection.commit_transaction().unwrap();
    assert_eq!(reader.load_sequence_rows().unwrap(), [assigned]);
    other.rollback_transaction().unwrap();
    assert_eq!(
        reader.load_sequence_rows().unwrap(),
        catalog.load_sequence_rows().unwrap()
    );
    assert_eq!(
        catalog
            .reserve_sequence_values("s", next.object_id, next.definition_generation)
            .unwrap(),
        SequenceReservationResult::Missing
    );
    assert!(catalog.drop_sequence_row("renamed").unwrap());
    assert!(catalog.drop_sequence_row("s").unwrap());
    let mut fresh = sequence("s", 4);
    fresh.options.min_value = None;
    fresh.options.max_value = None;
    assert!(catalog.create_sequence_row(&fresh).unwrap());
    assert_eq!(catalog.load_sequence_rows().unwrap(), [sequence("s", 4)]);
    assert_eq!(
        catalog
            .reserve_sequence_values("s", recreated.object_id, recreated.definition_generation)
            .unwrap(),
        SequenceReservationResult::Missing
    );
}

#[test]
fn native_sequence_budget_failure_does_not_stage_a_name_or_retire_a_definition() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    connection.begin_transaction().unwrap();
    let mut huge = sequence("s", 1);
    huge.security = large_sequence_security();
    assert!(catalog.create_sequence_row(&huge).is_err());
    assert!(catalog.load_sequence_rows().unwrap().is_empty());
    let original = sequence("s", 1);
    assert!(catalog.create_sequence_row(&original).unwrap());
    huge.definition_generation = [9; 16];
    assert!(catalog.replace_sequence_row(&huge).is_err());
    assert_eq!(
        catalog.load_sequence_rows().unwrap(),
        std::slice::from_ref(&original)
    );
    connection.commit_transaction().unwrap();
    assert_eq!(catalog.load_sequence_rows().unwrap(), [original]);
}

#[test]
fn native_sequence_commit_rejects_concurrent_aliases_of_the_same_incarnation() {
    let (connection, catalog) = memory(true);
    connection.begin_transaction().unwrap();
    assert!(catalog.create_sequence_row(&sequence("left", 1)).unwrap());
    let other = connection.new_session();
    let writer = Catalog::open(other).unwrap();
    let mut right = sequence("right", 1);
    right.definition_generation = [99; 16];
    assert!(writer.create_sequence_row(&right).unwrap());
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.load_sequence_rows().unwrap(), [right]);
}

#[test]
fn native_sequence_conversion_rejects_aliased_incarnations_atomically() {
    let (connection, catalog) = memory(false);
    let first = sequence("a", 1);
    let mut second = sequence("b", 1);
    second.definition_generation = [99; 16];
    catalog.create_sequence_row(&first).unwrap();
    catalog.create_sequence_row(&second).unwrap();
    assert!(connection
        .bind_native_records(VersionedSessionOptions::default())
        .is_err());
    assert_eq!(
        catalog.get_metadata("schema_version").unwrap().as_deref(),
        Some("48")
    );
    assert_eq!(
        catalog.load_sequence_rows().unwrap(),
        [first.clone(), second]
    );
    assert!(catalog.drop_sequence_row("b").unwrap());
    bind(&connection);
    assert_eq!(catalog.load_sequence_rows().unwrap(), [first]);
}
