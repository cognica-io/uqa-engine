//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cached bounds, stale writers and object-scoped value access.

use super::*;

/// Replace the row under a new allocation generation, with which a replacement stores its value state.
fn redefine(catalog: &Catalog, row: &mut SequenceRow) {
    row.definition_generation[0] += 1;
    assert!(catalog.replace_sequence_row(row).unwrap());
}

#[test]
fn sequence_reservations_keep_cached_bounds_cycles_and_full_width_values() {
    for native in [false, true] {
        let (connection, catalog) = memory(native);
        let mut row = sequence("s", 1);
        row.start = 5;
        row.current = 5;
        row.increment = 2;
        row.options.min_value = Some(3);
        row.options.max_value = Some(9);
        row.options.cycle = true;
        catalog.create_sequence_row(&row).unwrap();
        for (first, last, count) in [(5, 9, 3), (3, 7, 3), (9, 9, 1)] {
            let reserved = reserve(&catalog, &row);
            assert_eq!(
                (reserved.first_value, reserved.last_value, reserved.count),
                (first, last, count)
            );
            let stored = catalog.load_sequence_rows().unwrap().remove(0);
            assert_eq!(
                (stored.current, stored.called, stored.log_count),
                (last, true, reserved.log_count)
            );
        }
        for (current, increment, first, last) in [
            (i64::MAX - 2, 2, i64::MAX - 2, i64::MAX),
            (i64::MIN + 2, -2, i64::MIN + 2, i64::MIN),
            (i64::MAX, i64::MIN, i64::MAX, -1),
        ] {
            row.current = current;
            row.increment = increment;
            row.called = false;
            row.options.min_value = Some(i64::MIN);
            row.options.max_value = Some(i64::MAX);
            row.options.cache_size = i64::MAX;
            row.options.cycle = false;
            redefine(&catalog, &mut row);
            let reserved = reserve(&catalog, &row);
            assert_eq!(
                (reserved.first_value, reserved.last_value, reserved.count),
                (first, last, 2)
            );
            let previous = catalog.load_sequence_rows().unwrap();
            assert_eq!(
                catalog
                    .reserve_sequence_values("s", row.object_id, row.definition_generation)
                    .unwrap(),
                SequenceReservationResult::Exhausted
            );
            assert_eq!(catalog.load_sequence_rows().unwrap(), previous);
        }
        row.increment = 1;
        row.options.cache_size = 1;
        row.current = 7;
        row.called = false;
        row.log_count = 0;
        redefine(&catalog, &mut row);
        assert_eq!(
            catalog
                .set_sequence_value("s", row.object_id, row.definition_generation, 20, false, 0)
                .unwrap(),
            uqa_storage::SequenceSetValueResult::Set(20)
        );
        assert_eq!(reserve(&catalog, &row).first_value, 20);
        assert_eq!(
            catalog
                .set_sequence_value("s", row.object_id, row.definition_generation, 30, true, 0)
                .unwrap(),
            uqa_storage::SequenceSetValueResult::Set(30)
        );
        assert_eq!(reserve(&catalog, &row).first_value, 31);
        for id in [[0; 16], [2; 16]] {
            assert_eq!(
                catalog
                    .reserve_sequence_values("s", id, row.definition_generation)
                    .unwrap(),
                SequenceReservationResult::Missing
            );
            assert_eq!(
                catalog
                    .set_sequence_value("s", id, row.definition_generation, 100, false, 0)
                    .unwrap(),
                uqa_storage::SequenceSetValueResult::Missing
            );
        }
        assert_eq!(
            catalog
                .reserve_sequence_values("s", row.object_id, [0; 16])
                .unwrap(),
            SequenceReservationResult::DefinitionChanged
        );
        connection.begin_transaction().unwrap();
        row.increment = 0;
        redefine(&catalog, &mut row);
        super::lifecycle::reject(&connection, native, || {
            catalog.reserve_sequence_values("s", row.object_id, row.definition_generation)
        });
        assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 7);
        connection.rollback_transaction().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 31);
    }
}

#[test]
fn a_record_moves_only_from_the_position_its_caller_read() {
    for native in [false, true] {
        let (_connection, catalog) = memory(native);
        let row = sequence("s", 1);
        catalog.create_sequence_row(&row).unwrap();
        let logged = |value| SequenceValuePosition {
            current: value,
            called: true,
            log_count: 0,
        };
        let log = |expected, value| {
            catalog
                .log_sequence_values(
                    "s",
                    row.object_id,
                    row.definition_generation,
                    expected,
                    logged(value),
                )
                .unwrap()
        };
        assert_eq!(log((1, false), 33), SequenceLogResult::Logged);
        let record = catalog.load_sequence_rows().unwrap();
        // Nothing is written from a position the record has left, and the record it holds is reported.
        assert_eq!(log((1, false), 99), SequenceLogResult::Changed(logged(33)));
        assert_eq!(log((33, false), 99), SequenceLogResult::Changed(logged(33)));
        assert_eq!(
            catalog
                .log_sequence_values("s", row.object_id, [90; 16], (33, true), logged(99))
                .unwrap(),
            SequenceLogResult::DefinitionChanged
        );
        assert_eq!(
            catalog
                .log_sequence_values(
                    "s",
                    [2; 16],
                    row.definition_generation,
                    (33, true),
                    logged(99)
                )
                .unwrap(),
            SequenceLogResult::Missing
        );
        // The record is found by object identity, whatever name the caller knows the sequence by.
        assert_eq!(
            catalog
                .log_sequence_values(
                    "absent",
                    row.object_id,
                    row.definition_generation,
                    (1, false),
                    logged(99)
                )
                .unwrap(),
            SequenceLogResult::Changed(logged(33))
        );
        assert!(catalog
            .log_sequence_values(
                "s",
                row.object_id,
                row.definition_generation,
                (33, true),
                SequenceValuePosition {
                    log_count: -1,
                    ..logged(99)
                },
            )
            .is_err());
        assert_eq!(catalog.load_sequence_rows().unwrap(), record);
        assert_eq!(log((33, true), 66), SequenceLogResult::Logged);
        // A reservation continues after the record, as a sequence without a kept position does.
        assert_eq!(reserve(&catalog, &row).first_value, 67);
    }
}

#[test]
fn a_replacement_of_the_same_allocation_generation_keeps_the_value_state() {
    for native in [false, true] {
        let (_connection, catalog) = memory(native);
        let row = sequence("s", 1);
        catalog.create_sequence_row(&row).unwrap();
        assert_eq!(reserve(&catalog, &row).last_value, 3);
        let value_state = |catalog: &Catalog| {
            let stored = catalog.load_sequence_rows().unwrap().remove(0);
            (
                stored.start,
                stored.current,
                stored.called,
                stored.log_count,
            )
        };
        assert_eq!(value_state(&catalog), (1, 3, true, 32));
        // The row a session writes for an owner or privilege change carries whatever value state its registry holds.
        let mut replaced = row.clone();
        replaced.start = 7;
        replaced.current = 100;
        replaced.called = false;
        replaced.log_count = 5;
        assert!(catalog.replace_sequence_row(&replaced).unwrap());
        assert_eq!(value_state(&catalog), (7, 3, true, 32));
        assert_eq!(reserve(&catalog, &row).first_value, 4);
        // Unset identities name the stored object and generation, which is the same generation.
        replaced.object_id = [0; 16];
        replaced.definition_generation = [0; 16];
        if native {
            assert!(catalog.replace_sequence_row(&replaced).unwrap());
            assert_eq!(value_state(&catalog), (7, 6, true, 29));
        }
        // A new allocation generation is stored with the value state it comes with.
        replaced.object_id = row.object_id;
        replaced.definition_generation = [91; 16];
        assert!(catalog.replace_sequence_row(&replaced).unwrap());
        assert_eq!(value_state(&catalog), (7, 100, false, 5));
        assert_eq!(reserve(&catalog, &replaced).first_value, 100);
    }
}

#[test]
fn sequence_value_updates_reject_stale_definitions_without_writes() {
    for native in [false, true] {
        let (_connection, catalog) = memory(native);
        let row = sequence("s", 1);
        catalog.create_sequence_row(&row).unwrap();
        let unchanged = catalog.load_sequence_rows().unwrap();
        assert_eq!(
            catalog
                .set_sequence_value("s", row.object_id, [90; 16], 75, true, 0)
                .unwrap(),
            uqa_storage::SequenceSetValueResult::DefinitionChanged
        );
        assert_eq!(catalog.load_sequence_rows().unwrap(), unchanged);
    }
}

#[test]
fn native_sequence_conflicts_reject_stale_reservations_and_name_claims() {
    // A reservation conflicts with every other change of its generation's value record.
    for change in ["reserve", "replace", "drop"] {
        let (connection, catalog) = memory(true);
        let row = sequence("s", 1);
        catalog.create_sequence_row(&row).unwrap();
        connection.begin_transaction().unwrap();
        assert_eq!(reserve(&catalog, &row).first_value, 1);
        catalog.save_model("private", "discarded").unwrap();
        let other = connection.new_session();
        let writer = Catalog::open(other).unwrap();
        match change {
            "reserve" => assert_eq!(reserve(&writer, &row).first_value, 1),
            "replace" => {
                let mut updated = row.clone();
                updated.definition_generation = [8; 16];
                updated.current = 100;
                assert!(writer.replace_sequence_row(&updated).unwrap());
            }
            _ => assert!(writer.drop_sequence_row("s").unwrap()),
        }
        let expected = writer.load_sequence_rows().unwrap();
        assert!(connection.commit_transaction().is_err(), "{change}");
        assert_eq!(writer.load_sequence_rows().unwrap(), expected);
        assert_eq!(writer.load_model("private").unwrap(), None);
        connection.rollback_transaction().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap(), expected);
    }
    // A rename changes only the definition, so a reservation of another session commits with it.
    let (connection, catalog) = memory(true);
    let row = sequence("s", 1);
    catalog.create_sequence_row(&row).unwrap();
    connection.begin_transaction().unwrap();
    assert_eq!(reserve(&catalog, &row).first_value, 1);
    let writer = Catalog::open(connection.new_session()).unwrap();
    assert!(writer.rename_sequence_row("s", "renamed").unwrap());
    connection.commit_transaction().unwrap();
    let mut expected = row.clone();
    expected.relation = RelationIdentity::new("public", "renamed");
    (expected.current, expected.called, expected.log_count) = (3, true, 32);
    assert_eq!(writer.load_sequence_rows().unwrap(), [expected]);

    let (connection, catalog) = memory(true);
    connection.begin_transaction().unwrap();
    assert!(catalog.create_sequence_row(&sequence("same", 1)).unwrap());
    let other = connection.new_session();
    let writer = Catalog::open(other).unwrap();
    let winner = sequence("same", 2);
    assert!(writer.create_sequence_row(&winner).unwrap());
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.load_sequence_rows().unwrap(), [winner]);
}

#[test]
fn native_sequence_value_access_does_not_load_unrelated_catalog_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    let selected = sequence("small", 9);
    catalog.create_sequence_row(&selected).unwrap();
    let mut huge = sequence("unrelated", 1);
    huge.security = large_sequence_security();
    catalog.create_sequence_row(&huge).unwrap();
    drop(catalog);
    drop(connection);
    let limited = ManagedConnection::open(&path).unwrap();
    limited
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let catalog = Catalog::open(limited.clone()).unwrap();
    assert!(catalog.load_sequence_rows().is_err());
    assert_eq!(reserve(&catalog, &selected).first_value, 1);
    assert_eq!(
        catalog
            .set_sequence_value(
                "small",
                selected.object_id,
                selected.definition_generation,
                50,
                false,
                0
            )
            .unwrap(),
        uqa_storage::SequenceSetValueResult::Set(50)
    );
    assert_eq!(reserve(&catalog, &selected).last_value, 52);
    assert_eq!(
        catalog
            .reserve_sequence_values("small", selected.object_id, [90; 16])
            .unwrap(),
        SequenceReservationResult::DefinitionChanged
    );
    // The value record is found by identity, whatever name the caller knows the sequence by.
    let SequenceReservationResult::Reserved(renamed) = catalog
        .reserve_sequence_values(
            "unrelated",
            selected.object_id,
            selected.definition_generation,
        )
        .unwrap()
    else {
        panic!("expected a reservation by identity")
    };
    assert_eq!((renamed.first_value, renamed.last_value), (53, 55));
    limited
        .with_physical(|sql| {
            assert_eq!(
                sql.query_row(
                    "SELECT current FROM _uqa_mvcc_native_sequence_values WHERE object_id = ?1",
                    [selected.object_id.as_slice()],
                    |row| row.get::<_, i64>(0)
                )?,
                55
            );
            // Definitions keep the value state their generations started with.
            for name in ["small", "unrelated"] {
                assert_eq!(
                    sql.query_row(
                        "SELECT current FROM _sequences WHERE relation_name = ?1",
                        [name],
                        |row| row.get::<_, i64>(0)
                    )?,
                    1
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_sequence_value_changes_leave_catalog_cache_revisions() {
    let (_connection, catalog) = memory(true);
    let mut row = sequence("logged", 1);
    catalog.create_sequence_row(&row).unwrap();
    let registries = catalog.cache_revisions().unwrap().registries;
    // Value operations move the generation's value record, which no session's catalog cache holds.
    assert_eq!(reserve(&catalog, &row).last_value, 3);
    assert_eq!(
        catalog
            .log_sequence_values(
                &row.relation.qualified_name(),
                row.object_id,
                row.definition_generation,
                (3, true),
                SequenceValuePosition {
                    current: 40,
                    called: true,
                    log_count: 0,
                },
            )
            .unwrap(),
        SequenceLogResult::Logged
    );
    assert_eq!(
        catalog
            .set_sequence_value(
                &row.relation.qualified_name(),
                row.object_id,
                row.definition_generation,
                7,
                false,
                0
            )
            .unwrap(),
        uqa_storage::SequenceSetValueResult::Set(7)
    );
    assert_eq!(catalog.cache_revisions().unwrap().registries, registries);
    // A definition change still invalidates every session's catalog.
    row.security = SequenceSecurityRow::Bound(BoundSequenceSecurity::owner(RoleIdentity {
        oid: 20_002,
        object_id: [8; 16],
    }));
    assert!(catalog.replace_sequence_row(&row).unwrap());
    assert_ne!(catalog.cache_revisions().unwrap().registries, registries);
    let stored = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(
        (stored.current, stored.called, stored.log_count),
        (7, false, 0)
    );
}
