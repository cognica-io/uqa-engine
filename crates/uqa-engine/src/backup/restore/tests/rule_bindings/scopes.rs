//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const ORACLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/parity/pg18/backup_rule_scopes_oracle.expected.txt"
));

fn dependencies(engine: &Engine) {
    let mut actual = Vec::new();
    for (_, entries) in engine.durable.rules.read().iter() {
        for rule in entries.values() {
            for column in &rule.dependencies.as_ref().unwrap().columns {
                if !matches!(column.relation.name.as_str(), "items" | "scope_log") {
                    actual.push(format!(
                        "{}:{}.{}",
                        rule.definition.name, column.relation.name, column.column
                    ));
                }
            }
        }
    }
    actual.sort();
    assert_eq!(actual, ORACLE.lines().skip(8).collect::<Vec<_>>());
}

fn effects(engine: &Engine) {
    let output = engine.sql(
        "SELECT string_agg(kind || ':' || value::text, E'\\n' ORDER BY kind, value) AS observations FROM scope_log",
        &[],
    ).unwrap();
    assert_eq!(
        output.rows[0]["observations"],
        Value::Str(ORACLE.lines().take(8).collect::<Vec<_>>().join("\n")),
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_rule_scopes_preserve_distinct_aliases_and_quoted_correlations(
    #[case] provider: Provider,
) {
    assert_eq!(ORACLE.lines().count(), 15);
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, _) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_rule_scopes_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    source.sql(setup, &[]).unwrap();
    dependencies(&source);
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
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    dependencies(&restored);
    restored.sql("INSERT INTO items VALUES(1)", &[]).unwrap();
    effects(&restored);
    restored
        .sql("BEGIN; INSERT INTO items VALUES(2); ROLLBACK", &[])
        .unwrap();
    effects(&restored);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        dependencies(&reopened);
        effects(&reopened);
        reopened
            .sql("TRUNCATE scope_log; INSERT INTO items VALUES(3)", &[])
            .unwrap();
        effects(&reopened);
    }
}
