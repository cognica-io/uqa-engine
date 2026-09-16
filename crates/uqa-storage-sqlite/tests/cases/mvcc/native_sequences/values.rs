//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cached bounds, stale writers and object-scoped value access.

use super::*;

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
            catalog.replace_sequence_row(&row).unwrap();
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
        catalog.replace_sequence_row(&row).unwrap();
        assert_eq!(
            catalog
                .set_sequence_value("s", row.object_id, 20, false, 0)
                .unwrap(),
            Some(20)
        );
        assert_eq!(reserve(&catalog, &row).first_value, 20);
        assert_eq!(
            catalog
                .set_sequence_value("s", row.object_id, 30, true, 0)
                .unwrap(),
            Some(30)
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
                catalog.set_sequence_value("s", id, 100, false, 0).unwrap(),
                None
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
        catalog.replace_sequence_row(&row).unwrap();
        super::lifecycle::reject(&connection, native, || {
            catalog.reserve_sequence_values("s", row.object_id, row.definition_generation)
        });
        assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 7);
        connection.rollback_transaction().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 31);
    }
}

#[test]
fn native_sequence_conflicts_reject_stale_reservations_and_name_claims() {
    for change in ["reserve", "replace", "rename", "drop"] {
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
            "rename" => assert!(writer.rename_sequence_row("s", "renamed").unwrap()),
            _ => assert!(writer.drop_sequence_row("s").unwrap()),
        }
        let expected = writer.load_sequence_rows().unwrap();
        assert!(connection.commit_transaction().is_err(), "{change}");
        assert_eq!(writer.load_sequence_rows().unwrap(), expected);
        assert_eq!(writer.load_model("private").unwrap(), None);
        connection.rollback_transaction().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap(), expected);
    }
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
    huge.acl = Some(vec![SequenceAclEntry {
        role: "x".repeat(1024 * 1024),
        grantor: None,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }]);
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
            .set_sequence_value("small", selected.object_id, 50, false, 0)
            .unwrap(),
        Some(50)
    );
    assert_eq!(reserve(&catalog, &selected).last_value, 52);
    assert_eq!(
        catalog
            .reserve_sequence_values("small", selected.object_id, [90; 16])
            .unwrap(),
        SequenceReservationResult::DefinitionChanged
    );
    assert_eq!(
        catalog
            .reserve_sequence_values(
                "unrelated",
                selected.object_id,
                selected.definition_generation
            )
            .unwrap(),
        SequenceReservationResult::Missing
    );
    limited
        .with_physical(|sql| {
            assert_eq!(
                sql.query_row(
                    "SELECT current FROM _sequences WHERE relation_name = 'small'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                52
            );
            assert_eq!(
                sql.query_row(
                    "SELECT current FROM _sequences WHERE relation_name = 'unrelated'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
}
