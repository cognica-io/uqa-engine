//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::providers::{capture_provider, destination};
use super::*;
use uqa_sql::{
    ast::{CreateFunction, FunctionParamMode, FunctionReturns},
    routines::SQLUserFunction,
};

mod stored_defaults;

fn damage(definition: &mut CreateFunction, kind: &str) {
    match kind {
        "cost" => definition.cost = Some(0.0),
        "rows" => definition.rows = Some(0.0),
        "scalar_rows" => definition.rows = Some(2.0),
        "support" => definition.support = Some("missing_support".into()),
        "missing_language" => definition.language.clear(),
        "language" => definition.language = "missing_language".into(),
        "duplicate" => definition.params[1].name = definition.params[0].name.clone(),
        "early_duplicate" | "late_duplicate" => {
            let index = if kind == "early_duplicate" {
                1
            } else {
                definition.params.len() - 1
            };
            definition.params[index].name = definition.params[index - 1].name.clone();
            definition.returns = FunctionReturns::Scalar {
                type_name: "anyelement".into(),
            };
        }
        "output_default" => definition.params[1].mode = FunctionParamMode::Out,
        "variadic" => definition.params[1].mode = FunctionParamMode::Variadic,
        "variadic_domain" => {
            definition.params[1].mode = FunctionParamMode::Variadic;
            definition.params[1].type_name = "public.declaration_ints".into();
            definition.params[1].name = definition.params[0].name.clone();
        }
        "default_gap" => definition.params[1].default = None,
        "polymorphic_result" => {
            definition.returns = FunctionReturns::Scalar {
                type_name: "anyelement".into(),
            }
        }
        _ => unreachable!(),
    }
}

fn capture_changed(
    provider: Provider,
    path: &std::path::Path,
    repository: &LocalRepository,
    kind: &str,
) -> BackupRoot {
    let sql = if matches!(kind, "early_duplicate" | "late_duplicate") {
        let parameters = (0..96)
            .map(|index| format!("p{index:03} integer"))
            .collect::<Vec<_>>()
            .join(",");
        format!("CREATE FUNCTION saved({parameters}) RETURNS integer LANGUAGE SQL AS 'SELECT 1'")
    } else if kind == "variadic_domain" {
        "CREATE DOMAIN declaration_ints AS integer[]; CREATE FUNCTION saved(n integer,m integer) RETURNS integer LANGUAGE SQL AS 'SELECT 1'".into()
    } else {
        "CREATE FUNCTION saved(n integer DEFAULT 1,m integer DEFAULT 2) RETURNS integer LANGUAGE SQL AS 'SELECT 1'".into()
    };
    capture_definition(provider, path, repository, &sql, |definition| {
        damage(definition, kind);
    })
}

fn capture_definition(
    provider: Provider,
    path: &std::path::Path,
    repository: &LocalRepository,
    sql: &str,
    edit: impl FnOnce(&mut CreateFunction),
) -> BackupRoot {
    let source = provider.open(path);
    source.sql(sql, &[]).unwrap();
    if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let key = "sql_functions_json";
        let mut record: serde_json::Value =
            serde_json::from_str(&session.catalog.get_metadata(key).unwrap().unwrap()).unwrap();
        let selected = &mut record["definitions"]["public.saved"][0];
        let mut definition: CreateFunction = serde_json::from_value(selected.clone()).unwrap();
        edit(&mut definition);
        *selected = serde_json::to_value(definition).unwrap();
        session
            .catalog
            .set_metadata(key, &record.to_string())
            .unwrap();
        drop(session);
        capture_provider(persistent.as_ref(), repository)
    } else {
        {
            let mut routines = source.durable.sql_user_functions.write();
            let original = &routines["public.saved"][0];
            let mut definition = original.def.clone();
            edit(&mut definition);
            let changed = SQLUserFunction::new(definition, original.body.clone()).shared();
            routines
                .insert("public.saved".into(), vec![changed])
                .unwrap();
        }
        let root = capture(&source, repository, None);
        drop(source);
        root
    }
}

fn assert_array_calls(engine: &Engine) {
    let result = engine
        .sql(
            "SELECT saved(1,2,3) AS expanded, saved(VARIADIC ARRAY[7,8]) AS explicit",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["expanded"], Value::Int(3));
    assert_eq!(result.rows[0]["explicit"], Value::Int(2));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_variadic_catalog_array_alias_accepts_expanded_and_explicit_calls(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source.sql("CREATE FUNCTION saved(VARIADIC n pg_catalog._int4) RETURNS integer LANGUAGE SQL AS 'SELECT cardinality($1)'", &[]).unwrap();
    assert_array_calls(&source);
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
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
    assert_array_calls(&engine);
    drop(engine);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        assert_array_calls(&provider.open(&path.join("database")));
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_declarations_reject_invalid_original_signatures(#[case] provider: Provider) {
    for (kind, expected) in [
        ("cost", "COST must be positive"),
        ("rows", "ROWS must be positive"),
        ("scalar_rows", "ROWS is not applicable"),
        ("support", "missing_support"),
        ("missing_language", "no language specified"),
        ("language", "language"),
        ("duplicate", "used more than once"),
        ("output_default", "only input parameters"),
        ("variadic", "VARIADIC parameter must be an array"),
        ("variadic_domain", "VARIADIC parameter must be an array"),
        ("default_gap", "must also have defaults"),
        ("polymorphic_result", "cannot determine result data type"),
    ] {
        reject(provider, kind, expected);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn duplicate_parameter_positions_preserve_restore_windows_and_precede_invalid_results(
    #[case] provider: Provider,
) {
    let early = reject(
        provider,
        "early_duplicate",
        "parameter name \"p000\" used more than once",
    );
    let late = reject(
        provider,
        "late_duplicate",
        "parameter name \"p094\" used more than once",
    );
    println!("{provider:?} early_validation_windows={early} late_validation_windows={late}");
    assert!(
        late > early,
        "{provider:?}: {early} early vs {late} late validation windows"
    );
}

fn reject(provider: Provider, kind: &str, expected: &str) -> usize {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let root = capture_changed(
        provider,
        &directory.path().join("source"),
        &repository,
        kind,
    );
    let path = directory.path().join("restored");
    reject_root(provider, &repository, root, &path, kind, expected)
}

fn reject_root(
    provider: Provider,
    repository: &LocalRepository,
    root: BackupRoot,
    path: &std::path::Path,
    kind: &str,
    expected: &str,
) -> usize {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = Engine::new()
        .begin_restore(
            repository,
            root.backup(),
            destination(provider, path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    let mut validation_windows = 0;
    let error = loop {
        match job.advance(1024) {
            Err(error) => break error,
            Ok(RestorePhase::Reading) => (),
            Ok(RestorePhase::Validating) => validation_windows += 1,
            Ok(phase) => panic!("{provider:?} {kind}: invalid declaration reached {phase:?}"),
        }
    };
    assert!(
        error.to_string().contains(expected),
        "{provider:?} {kind}: {error}"
    );
    drop(error);
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert!(matches!(job.advance(1024), Err(BackupError::Finished)));
    assert!(job.cancel().unwrap().is_none());
    assert_eq!(job.phase(), RestorePhase::Failed);
    drop(job);
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
    validation_windows
}
