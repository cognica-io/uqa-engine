//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn definitions(engine: &Engine, process_local: bool) -> serde_json::Value {
    let rules = engine.durable.rules.read();
    serde_json::to_value(
        rules
            .iter()
            .map(|(owner, entries)| {
                (
                    owner,
                    entries
                        .values()
                        .map(|rule| {
                            let mut value = serde_json::to_value(rule).unwrap();
                            if process_local {
                                // Ordinary persistent open reallocates process-local row identities together with every reference.
                                let binding = &value["condition_binding"];
                                let old = binding["old_relation"].clone();
                                let new = binding["new_relation"].clone();
                                if !old.is_null() && !new.is_null() {
                                    assert_ne!(old, new);
                                }
                                row_names(&mut value["condition_plan"], &old, &new);
                                if !old.is_null() {
                                    value["condition_binding"]["old_relation"] = "OLD".into();
                                }
                                if !new.is_null() {
                                    value["condition_binding"]["new_relation"] = "NEW".into();
                                }
                            }
                            value
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn row_names(value: &mut serde_json::Value, old: &serde_json::Value, new: &serde_json::Value) {
    if let Some(reference) = value.get_mut("InternalColumn") {
        let relation = &mut reference["relation"];
        *relation = if !old.is_null() && relation == old {
            "OLD".into()
        } else if !new.is_null() && relation == new {
            "NEW".into()
        } else {
            panic!("unowned captured reference: {relation}");
        };
        return;
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .for_each(|value| row_names(value, old, new)),
        serde_json::Value::Object(values) => values
            .values_mut()
            .for_each(|value| row_names(value, old, new)),
        _ => (),
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_rule_bindings_preserve_correlated_rows_and_live_column_layout(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, exercise) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_rule_bindings_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    source.sql(setup, &[]).unwrap();
    let process_local = !matches!(provider, Provider::Memory);
    let expected = definitions(&source, process_local);
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    assert_eq!(definitions(&restored, process_local), expected);
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    {
        let output = restored.sql(exercise, &[]).unwrap();
        let Value::Str(observations) = &output.rows[0]["observations"] else {
            panic!("rule observations");
        };
        assert_eq!(
            observations.trim(),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/parity/pg18/backup_rule_bindings_oracle.expected.txt"
            ))
            .trim()
        );
    }
    restored
        .sql("BEGIN; UPDATE items SET value=99; ROLLBACK", &[])
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT value FROM items WHERE item_id=2", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(21)
    );
    assert_eq!(definitions(&restored, process_local), expected);
    let rows = restored
        .sql("SELECT entry FROM rule_log ORDER BY entry", &[])
        .unwrap()
        .rows;
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(definitions(&reopened, process_local), expected);
        assert_eq!(
            reopened
                .sql("SELECT entry FROM rule_log ORDER BY entry", &[])
                .unwrap()
                .rows,
            rows
        );
        reopened
            .sql("UPDATE items SET value=value+1 WHERE item_id=2", &[])
            .unwrap();
        assert_eq!(
            reopened
                .sql(
                    "SELECT count(*) AS n FROM rule_log WHERE entry='change:21:22'",
                    &[]
                )
                .unwrap()
                .rows[0]["n"],
            Value::Int(1)
        );
    }
}
