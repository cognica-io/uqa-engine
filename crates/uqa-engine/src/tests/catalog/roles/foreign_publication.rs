//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private foreign definitions retain their identity and security beside fresh peer metadata.

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use uqa_core::{RelationIdentity, Value};

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn private_foreign_definitions_survive_refresh_commit_and_undo(
    #[case] provider: usize,
    #[values("REPEATABLE READ", "SERIALIZABLE")] isolation: &str,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO before_foreign; COMMIT")] finish: &str,
) {
    let (directory, first, second) = sessions(provider);
    sql(
        &first,
        "CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw",
    );
    for name in ["replaced", "renamed_from", "dropped", "recreated", "peer"] {
        let columns = if matches!(name, "replaced" | "peer") {
            "v int, extra text"
        } else {
            "v int"
        };
        sql(
            &first,
            &format!("CREATE FOREIGN TABLE {name}({columns}) SERVER remote"),
        );
    }
    sql(
        &first,
        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_foreign"),
    );
    sql(&first, "ALTER FOREIGN TABLE replaced DROP COLUMN extra; GRANT SELECT(v) ON replaced TO PUBLIC; ALTER FOREIGN TABLE renamed_from RENAME TO renamed_to; DROP FOREIGN TABLE dropped; DROP FOREIGN TABLE recreated; CREATE FOREIGN TABLE recreated(v text) SERVER remote; CREATE FOREIGN TABLE created(v int) SERVER remote");
    let private = first.durable.foreign_tables.snapshot();
    let private_security = first.durable.foreign_table_security.snapshot();
    sql(&second, "ALTER FOREIGN TABLE peer DROP COLUMN extra");
    let committed = second.durable.foreign_tables.snapshot();
    let committed_security = second.durable.foreign_table_security.snapshot();
    first.synchronize_catalog_registries().unwrap();
    let current = first.durable.foreign_tables.snapshot();
    let security = first.durable.foreign_table_security.snapshot();
    let definition = |table: &uqa_execution::catalog::foreign::StoredForeignTable| {
        (
            table.name.clone(),
            table.persistence,
            table.server_name.clone(),
            table.options.clone(),
            table.schema_json().unwrap(),
        )
    };
    for name in ["replaced", "renamed_to", "recreated", "created"] {
        let relation = RelationIdentity::new("public", name);
        assert_eq!(
            definition(&current[&relation]),
            definition(&private[&relation]),
            "{name} after peer publication"
        );
        assert_eq!(security[&relation], private_security[&relation]);
    }
    for name in ["dropped", "renamed_from"] {
        let relation = RelationIdentity::new("public", name);
        assert!(!current.contains_key(&relation), "{name}");
        assert!(!security.contains_key(&relation), "{name}");
    }
    let peer = RelationIdentity::new("public", "peer");
    assert_eq!(definition(&current[&peer]), definition(&committed[&peer]));
    assert_eq!(security[&peer], committed_security[&peer]);
    sql(&first, finish);
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/private_foreign_refresh_oracle.expected.json"
    )))
    .unwrap();
    let expected = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["isolation"] == isolation && case["finish"] == finish)
        .unwrap();
    let check = |engine: &crate::Engine| {
        let columns = sql(engine, "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod) AS type FROM pg_class c JOIN pg_attribute a ON a.attrelid=c.oid WHERE c.relnamespace='public'::regnamespace AND c.relkind='f' AND a.attnum>0 AND NOT a.attisdropped ORDER BY c.relname, a.attnum")
            .rows.into_iter().map(|row| {
                ["relname", "attname", "type"].map(|field| {
                    let Value::Str(value) = &row[field] else { panic!("text catalog field {field}"); };
                    value.as_str()
                }).join("|")
            }).collect::<Vec<_>>();
        assert_eq!(serde_json::json!(columns), expected["columns"]);
    };
    check(&first);
    check(&second);
    drop(second);
    drop(first);
    check(&reopen(provider, &directory.path().join("table-locks.db")));
}
