//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed data snapshots retain private view definitions beside fresh committed catalog entries.

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use uqa_core::{RelationIdentity, Value};

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn private_view_definitions_survive_refresh_commit_and_undo(
    #[case] provider: usize,
    #[values("REPEATABLE READ", "SERIALIZABLE")] isolation: &str,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO before_views; COMMIT")] finish: &str,
) {
    let (directory, first, second) = sessions(provider);
    sql(&first, "CREATE VIEW replaced AS SELECT 1 AS v; CREATE VIEW renamed_from AS SELECT 2 AS v; CREATE VIEW dropped AS SELECT 3 AS v; CREATE MATERIALIZED VIEW saved AS SELECT v FROM t; CREATE VIEW peer AS SELECT 5 AS v");
    sql(
        &first,
        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_views"),
    );
    sql(&first, "CREATE OR REPLACE VIEW replaced AS SELECT 11 AS v; ALTER VIEW renamed_from RENAME TO renamed_to; DROP VIEW dropped; UPDATE t SET v = 44; REFRESH MATERIALIZED VIEW saved; CREATE VIEW created AS SELECT 6 AS v");
    let private = first.durable.views.snapshot();
    sql(&second, "CREATE OR REPLACE VIEW peer AS SELECT 55 AS v");
    let committed = second.durable.views.snapshot();
    // Exercise a catalog inquiry inside the pinned command before a later SQL boundary can repair a lost registry entry.
    first.synchronize_catalog_registries().unwrap();
    let current = first.durable.views.snapshot();
    for name in ["replaced", "renamed_to", "saved", "created"] {
        let relation = RelationIdentity::new("public", name);
        assert_eq!(
            serde_json::to_value(&current[&relation]).unwrap(),
            serde_json::to_value(&private[&relation]).unwrap(),
            "{name} after peer publication"
        );
        assert_eq!(current[&relation].security, private[&relation].security);
    }
    for name in ["dropped", "renamed_from"] {
        assert!(
            !current.contains_key(&RelationIdentity::new("public", name)),
            "{name}"
        );
    }
    let peer = RelationIdentity::new("public", "peer");
    assert_eq!(
        serde_json::to_value(&current[&peer]).unwrap(),
        serde_json::to_value(&committed[&peer]).unwrap(),
    );
    assert_eq!(private[&peer].object_id, committed[&peer].object_id);
    sql(&first, finish);
    let kept = finish == "COMMIT";
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/private_view_refresh_oracle.expected.json"
    )))
    .unwrap();
    let expected = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["isolation"] == isolation && case["finish"] == finish)
        .unwrap();
    let check = |engine: &crate::Engine| {
        let values = [
            "replaced",
            if kept { "renamed_to" } else { "renamed_from" },
            "saved",
            "peer",
        ]
        .map(|name| {
            let Value::Int(value) = sql(engine, &format!("SELECT v FROM {name}")).rows[0]["v"]
            else {
                panic!("integer fixture: {name}");
            };
            value.to_string()
        });
        assert_eq!(values.join("|"), expected["values"][0].as_str().unwrap());
        let names = sql(engine, "SELECT relname FROM pg_class WHERE relname IN ('created','dropped','renamed_from','renamed_to') ORDER BY relname")
            .rows.into_iter().map(|row| {
                let Value::Str(name) = &row["relname"] else { panic!("relation name"); };
                name.clone()
            }).collect::<Vec<_>>();
        assert_eq!(serde_json::json!(names), expected["names"]);
    };
    check(&first);
    drop(second);
    drop(first);
    check(&reopen(provider, &directory.path().join("table-locks.db")));
}
