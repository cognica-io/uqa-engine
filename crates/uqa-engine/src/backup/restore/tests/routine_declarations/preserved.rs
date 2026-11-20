//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::routines::RoutineBody;

fn observations(engine: &Engine) -> String {
    let (_, queries) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_declarations_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    let mut actual = String::new();
    for sql in queries.split(';').filter(|sql| !sql.trim().is_empty()) {
        let result = engine.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{sql}");
        match result.rows[0].values().next().unwrap() {
            Value::Int(value) => actual.push_str(&value.to_string()),
            Value::Str(value) => actual.push_str(value),
            other => panic!("unexpected declaration output: {other:?}"),
        }
        actual.push('\n');
    }
    actual
}

fn definitions(engine: &Engine) -> Vec<serde_json::Value> {
    engine
        .durable
        .sql_user_functions
        .read()
        .iter()
        .flat_map(|(_, entries)| {
            entries
                .iter()
                .map(|routine| serde_json::to_value(&routine.def).unwrap())
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_declarations_preserve_modes_domain_arrays_and_lazy_bodies(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, _) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_declarations_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    let expected = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_declarations_oracle.expected.txt"
    ));
    source.sql(setup, &[]).unwrap();
    let array = std::iter::repeat_n("abs(-1)", 256)
        .collect::<Vec<_>>()
        .join(",");
    source.sql(&format!("CREATE FUNCTION wide_default(v integer[] DEFAULT ARRAY[{array}]) RETURNS integer LANGUAGE SQL AS 'SELECT cardinality(v)'"), &[]).unwrap();
    let check_default = |engine: &Engine| {
        assert_eq!(
            engine.sql("SELECT wide_default() AS n", &[]).unwrap().rows[0]["n"],
            Value::Int(256)
        );
    };
    let original = definitions(&source);
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            super::super::providers::destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    assert_eq!(definitions(&restored), original);
    assert!(matches!(
        restored.durable.sql_user_functions.read()["public.declaration_lazy"][0].body,
        RoutineBody::Source
    ));
    assert!(matches!(
        restored.durable.sql_user_functions.read()["public.declaration_trigger_argument"][0].body,
        RoutineBody::Source
    ));
    assert_eq!(observations(&restored), expected);
    check_default(&restored);
    restored.sql("BEGIN; CREATE FUNCTION declaration_rolled_back() RETURNS integer LANGUAGE SQL AS 'SELECT 1'; ROLLBACK", &[]).unwrap();
    assert!(!restored
        .durable
        .sql_user_functions
        .read()
        .contains_key("public.declaration_rolled_back"));
    assert_eq!(definitions(&restored), original);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(observations(&reopened), expected);
        check_default(&reopened);
        assert_eq!(definitions(&reopened), original);
    }
}
