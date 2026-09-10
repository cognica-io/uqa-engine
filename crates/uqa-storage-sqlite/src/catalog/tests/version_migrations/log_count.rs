//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn migration_adds_the_sequence_log_counter_without_changing_values() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_log_count"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [41; 16],
            definition_generation: [42; 16],
            start: 7,
            increment: 2,
            current: 11,
            called: true,
            log_count: 19,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN log_count", [])?;
            database.execute(
                "UPDATE _metadata SET value = '31' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let row = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(row.current, 11);
    assert!(row.called);
    assert_eq!(row.log_count, 0);
    upgraded
        .set_sequence_value("public.legacy_log_count", [41; 16], 13, true, 17)
        .unwrap();
    let row = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(row.current, 13);
    assert_eq!(row.log_count, 17);
}
