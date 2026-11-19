//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::backup::restore::tests::persistent::protected_repository;

const DECLARATION: &str = "CREATE FUNCTION saved(n bigint DEFAULT 0, m bigint DEFAULT 0) RETURNS bigint LANGUAGE sql AS 'SELECT n * 10 + m'";

fn replace_defaults(definition: &mut CreateFunction, first: &str, last: &str) {
    let uqa_sql::Statement::CreateFunction(mut input) = uqa_sql::compile(&format!(
        "CREATE FUNCTION saved(n bigint DEFAULT {first}, m bigint DEFAULT {last}) RETURNS bigint LANGUAGE sql AS 'SELECT n * 10 + m'"
    )).unwrap().remove(0) else { unreachable!() };
    for (to, from) in definition.params.iter_mut().zip(&mut input.params) {
        to.default = from.default.take();
    }
}

fn check_defaults(engine: &Engine) {
    let result = engine.sql("SELECT saved() AS both_defaults, saved(8) AS second_default, saved(m => 4) AS first_default", &[]).unwrap();
    let row = &result.rows[0];
    assert_eq!(row["both_defaults"], Value::Int(71));
    assert_eq!(row["second_default"], Value::Int(81));
    assert_eq!(row["first_default"], Value::Int(74));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn stored_query_and_window_defaults_preserve_public_restore_diagnostics(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = protected_repository(&directory.path().join("repository"));
    let archive = |case: &str, first: &str, last: &str| {
        capture_definition(
            provider,
            &directory.path().join(case),
            &repository,
            DECLARATION,
            |definition| replace_defaults(definition, first, last),
        )
    };
    let root = archive("source", "7", "1");
    let control = StorageReadControl::with_limit(32 << 20);
    let path = directory.path().join("restored");
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let engine = job.publish().unwrap();
    drop(job);
    repository.remove_backup(root.backup(), &control).unwrap();
    check_defaults(&engine);
    drop(engine);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        check_defaults(&provider.open(&path.join("database")));
    }
    let rows = std::iter::repeat_n("(7)", 1024)
        .collect::<Vec<_>>()
        .join(",");
    let order = std::iter::repeat_n("1", 1024).collect::<Vec<_>>().join(",");
    let query = format!("(SELECT v FROM (VALUES {rows}) AS s(v) LIMIT 1)");
    let window = format!("row_number() OVER (ORDER BY {order})");
    let early = format!("{query} + absent_early");
    let late = format!("{window} + absent_late");
    let query_error =
        "schema expression contains a subquery whose dependencies cannot be rewritten safely";
    // A memory image validates the captured routine registry. Durable format-3 declarations first resolve their default names during private open.
    let (early_error, late_error) = if matches!(provider, Provider::Memory) {
        (query_error, query_error)
    } else {
        (
            "column \"absent_early\" does not exist",
            "column \"absent_late\" does not exist",
        )
    };
    for (case, first, last, expected) in [
        ("query", query.as_str(), "1", query_error),
        (
            "window",
            "7",
            window.as_str(),
            "invalid native routine default binding: non-scalar expression",
        ),
        ("first", early.as_str(), late.as_str(), early_error),
        ("last", query.as_str(), late.as_str(), late_error),
    ] {
        let root = archive(case, first, last);
        let target = directory.path().join(format!("{case}-target"));
        reject_root(provider, &repository, root, &target, case, expected);
    }
}
