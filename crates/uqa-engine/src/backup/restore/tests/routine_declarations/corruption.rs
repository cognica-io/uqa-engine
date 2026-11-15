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

fn damage(definition: &mut CreateFunction, kind: &str) {
    match kind {
        "language" => definition.language = "missing_language".into(),
        "duplicate" => definition.params[1].name = definition.params[0].name.clone(),
        "output_default" => definition.params[1].mode = FunctionParamMode::Out,
        "variadic" => definition.params[1].mode = FunctionParamMode::Variadic,
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
    let source = provider.open(path);
    source.sql("CREATE FUNCTION saved(n integer DEFAULT 1,m integer DEFAULT 2) RETURNS integer LANGUAGE SQL AS 'SELECT 1'", &[]).unwrap();
    if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let key = "sql_functions_json";
        let mut record: serde_json::Value =
            serde_json::from_str(&session.catalog.get_metadata(key).unwrap().unwrap()).unwrap();
        let selected = &mut record["definitions"]["public.saved"][0];
        let mut definition: CreateFunction = serde_json::from_value(selected.clone()).unwrap();
        damage(&mut definition, kind);
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
            damage(&mut definition, kind);
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

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_declarations_reject_invalid_original_signatures(#[case] provider: Provider) {
    for (kind, expected) in [
        ("language", "language"),
        ("duplicate", "used more than once"),
        ("output_default", "only input parameters"),
        ("variadic", "VARIADIC parameter must be an array"),
        ("default_gap", "must also have defaults"),
        ("polymorphic_result", "cannot determine result data type"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let root = capture_changed(
            provider,
            &directory.path().join("source"),
            &repository,
            kind,
        );
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
        let error = loop {
            match job.advance(1024) {
                Err(error) => break error,
                Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
                Ok(phase) => panic!("{provider:?} {kind}: invalid declaration reached {phase:?}"),
            }
        };
        assert!(
            error.to_string().contains(expected),
            "{provider:?} {kind}: {error}"
        );
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
    }
}
