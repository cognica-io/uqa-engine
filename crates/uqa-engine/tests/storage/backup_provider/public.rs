//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::backup::{
    BackupCapabilities, BackupCapturePhase, BackupError, BackupJob, BackupOptions,
};
use uqa_storage::backup::{
    BackupFunctionKind, BackupReadStep, BackupSourceStep, DecodedMemoryImageAddress,
    DecodedMemoryTableUnit, MemoryImageImportReader,
};

fn repository(path: &Path, control: &StorageReadControl, encrypted: bool) -> LocalRepository {
    LocalRepository::create(
        path,
        if encrypted {
            Protection::Encrypted {
                domain: ProtectionId::from_bytes([17; 16]).unwrap(),
                key: BackupKey::new([19; 32]),
            }
        } else {
            Protection::Plaintext
        },
        BackupLimits::new(4096, 4096).unwrap(),
        control,
    )
    .unwrap()
}

fn finish(mut job: BackupJob<'_>, mut writer: Option<&Engine>) -> BackupRoot {
    while job.advance(7, 1024).unwrap() == BackupCaptureStep::Pending {
        if job.progress().source.copied_payload_bytes > 0 {
            if let Some(writer) = writer.take() {
                assert_eq!(job.progress().phase, BackupCapturePhase::Capturing);
                sql(
                    writer,
                    "UPDATE archive.docs SET title = 'after public cut' WHERE id = 1",
                );
            }
        }
    }
    assert!(writer.is_none(), "writer ran between source windows");
    assert_eq!(job.progress().phase, BackupCapturePhase::Prepared);
    let root = job.publish().unwrap();
    assert_eq!(job.publish().unwrap(), root);
    assert_eq!(job.completed(), Some(root));
    root
}

#[rstest::rstest]
#[case::native_plain(Provider::Native, 0)]
#[case::native_encrypted(Provider::Native, 1)]
#[case::native_compressed(Provider::Native, 2)]
#[case::native_compressed_encrypted(Provider::Native, 3)]
#[case::kv_plain(Provider::KV, 0)]
#[case::kv_encrypted(Provider::KV, 1)]
#[case::kv_compressed(Provider::KV, 2)]
#[case::kv_compressed_encrypted(Provider::KV, 3)]
#[case::redb(Provider::Redb, 0)]
fn public_backup_jobs_preserve_committed_images_after_engine_and_source_loss(
    #[case] kind: Provider,
    #[case] mode: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let provider = kind.open(&source.join("database"), mode);
    let session = provider.open_session().unwrap();
    let backend_only = Engine::from_persistent_backends(session.catalog, session.backend).unwrap();
    assert_eq!(
        backend_only.backup_capabilities(),
        BackupCapabilities::UNSUPPORTED
    );
    drop(backend_only);
    let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
    assert_eq!(
        engine.backup_capabilities(),
        BackupCapabilities {
            export: true,
            import: true,
            snapshot_fingerprints: true,
        }
    );
    setup(&engine);
    let original = observe(&engine, provider.as_ref());
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, true);
    let options = BackupOptions::new(32 << 20, 16 << 20);
    sql(&engine, "BEGIN");
    sql(
        &engine,
        "UPDATE archive.docs SET title = 'private' WHERE id = 2",
    );
    sql(
        &engine,
        "CREATE TABLE archive.private_definition (value INTEGER)",
    );
    let job = engine.begin_backup(&repository, options, &control).unwrap();
    assert_eq!(job.cut().format.provider(), kind.image());
    assert_eq!(engine.transaction_depth(), 1);
    assert_eq!(
        sql(&engine, "SELECT title FROM archive.docs WHERE id = 2")[0]["title"],
        Value::Str("private".into())
    );
    sql(&engine, "ROLLBACK");
    let writer = Engine::from_persistent_provider(provider.clone()).unwrap();
    let full = finish(job, Some(&writer));
    let changed = observe(&writer, provider.as_ref());
    assert_ne!(original, changed);
    let child_job = engine
        .begin_backup(
            &repository,
            BackupOptions {
                parent: Some(full.backup()),
                ..options
            },
            &control,
        )
        .unwrap();
    drop((engine, writer, provider));
    let child = finish(child_job, None);
    assert_eq!(control.memory().used(), 0);
    std::fs::remove_dir_all(source).unwrap();
    restore::verify_engine(
        &repository,
        full,
        &directory.path().join("full"),
        kind,
        mode,
        &original,
        &control,
    );
    repository.remove_backup(full.backup(), &control).unwrap();
    restore::verify_engine(
        &repository,
        child,
        &directory.path().join("child"),
        kind,
        (mode + 1) % 4,
        &changed,
        &control,
    );
}

fn capture_report(mut job: BackupJob<'_>) -> (BackupRoot, BackupCaptureReport) {
    while job.advance(3, 4096).unwrap() == BackupCaptureStep::Pending {}
    let report = job.report().unwrap();
    (job.publish().unwrap(), report)
}

fn function_section_bytes(
    repository: &LocalRepository,
    root: BackupRoot,
    control: &StorageReadControl,
) -> u64 {
    let lease = repository.open_backup(root.backup(), control).unwrap();
    let mut input = lease.read_image(16 << 20, control).unwrap();
    assert_eq!(input.advance(1024).unwrap(), BackupReadStep::Unit);
    let entry = input.current_unit().unwrap();
    assert_eq!(entry.kind(), UnitKind::FunctionReferences);
    entry.decoded_bytes()
}

#[rstest::rstest]
#[case::memory(None)]
#[case::native(Some(Provider::Native))]
#[case::sqlite_kv(Some(Provider::KV))]
#[case::redb(Some(Provider::Redb))]
fn incremental_fingerprints_preserve_a_pinned_sparse_image_and_measure_actual_reads(
    #[case] kind: Option<Provider>,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = kind.map_or_else(Engine::new, |kind| {
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), 0)).unwrap()
    });
    sql(
        &engine,
        "CREATE TABLE archive_rows (id INTEGER PRIMARY KEY, body TEXT)",
    );
    sql(
        &engine,
        "INSERT INTO archive_rows VALUES (1,repeat('a',65536)),(2,repeat('b',65536))",
    );
    let control = StorageReadControl::with_limit(32 << 20);
    let repository = repository(&directory.path().join("repository"), &control, false);
    let options = BackupOptions::new(32 << 20, 16 << 20);
    let capture = |parent| {
        engine
            .begin_backup(&repository, BackupOptions { parent, ..options }, &control)
            .unwrap()
    };
    // Keep automatic statistics publication outside the two cuts whose inventories are compared.
    if kind.is_some() {
        sql(
            &engine,
            "BEGIN; LOCK TABLE archive_rows IN ACCESS EXCLUSIVE MODE",
        );
    }
    let (parent, full) = capture_report(capture(None));
    assert_eq!(
        full.image.hashed_payload_bytes,
        full.inventory.decoded_bytes
    );
    let (unchanged, steady) = capture_report(capture(Some(parent.backup())));
    assert_eq!(unchanged.summary(), parent.summary());
    assert_eq!(steady.image.written_units, 0);
    if kind.is_none() {
        // This cut has the 33-byte OID header and an empty declaration section.
        let section = function_section_bytes(&repository, unchanged, &control);
        assert_eq!(section, 9);
        assert_eq!(
            steady.image.hashed_payload_bytes,
            33 + section,
            "{steady:?}"
        );
    }
    assert_eq!(
        steady.image.written_object_bytes,
        unchanged.inventory().object_bytes()
    );
    assert!(
        steady.image.certified_reused_bytes >= 2 * 65536,
        "{steady:?}"
    );
    if kind.is_some() {
        sql(&engine, "ROLLBACK");
    }
    sql(
        &engine,
        "UPDATE archive_rows SET body=repeat('c',65536) WHERE id=2",
    );
    assert_eq!(
        sql(
            &engine,
            "SELECT body = repeat('c',65536) AS changed FROM archive_rows WHERE id=2"
        )[0]["changed"],
        Value::Bool(true)
    );
    if kind.is_some() {
        sql(
            &engine,
            "BEGIN; LOCK TABLE archive_rows IN ACCESS EXCLUSIVE MODE",
        );
    }
    let sparse = capture(Some(parent.backup()));
    let independent = capture(None);
    if kind.is_some() {
        sql(&engine, "ROLLBACK");
    }
    sql(
        &engine,
        "UPDATE archive_rows SET body=repeat('d',65536) WHERE id=1",
    );
    let (child, report) = capture_report(sparse);
    let (reference, _) = capture_report(independent);
    assert_eq!(child.summary(), reference.summary());
    assert_ne!(child.summary(), parent.summary());
    assert!(report.source.visited_positions > 0 && report.source.metadata_read_bytes > 0);
    assert!(report.image.certified_reused_bytes >= 65536, "{report:?}");
    assert!(
        report.source.copied_payload_bytes >= 65536 && report.image.written_object_bytes > 0,
        "{report:?}"
    );
    assert_eq!(
        report.image.hashed_payload_bytes + report.image.certified_reused_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.image.reused_payload_bytes + report.image.written_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(None)]
#[case::native(Some(Provider::Native))]
#[case::sqlite_kv(Some(Provider::KV))]
#[case::redb(Some(Provider::Redb))]
fn incremental_capture_reuses_provider_units_and_recomputes_the_registered_function_section(
    #[case] kind: Option<Provider>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use BackupFunctionKind::{AbsentAggregate, AbsentScalar, AbsentTable, Scalar};
    let directory = tempfile::tempdir().unwrap();
    let engine = kind.map_or_else(Engine::new, |kind| {
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), 0)).unwrap()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut registered = engine
        .register_scalar_function_retained(
            "abs",
            uqa_engine::SQLFunctionOptions::default(),
            move |_: &[Value]| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Int(17))
            },
        )
        .unwrap();
    sql(&engine, "CREATE VIEW saved_function AS SELECT abs(-5) AS v");
    sql(&engine, "CREATE TABLE payload(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO payload VALUES (1, repeat('p',65536))");
    if kind.is_some() {
        sql(
            &engine,
            "BEGIN; LOCK TABLE payload IN ACCESS EXCLUSIVE MODE",
        );
    }
    let control = StorageReadControl::with_limit(32 << 20);
    let repository = repository(&directory.path().join("backup"), &control, false);
    let start = |parent| {
        let mut options = BackupOptions::new(32 << 20, 16 << 20);
        options.parent = parent;
        engine.begin_backup(&repository, options, &control).unwrap()
    };
    let before = calls.load(Ordering::SeqCst);
    let (parent, _) = capture_report(start(None));
    let pinned = start(Some(parent.backup()));
    registered.unregister().unwrap();
    let (steady, unchanged) = capture_report(pinned);
    assert_eq!(steady.summary(), parent.summary());
    assert_eq!(unchanged.image.written_units, 0);
    let mut next = start(Some(parent.backup()));
    // Until the first unit completes, all reported source positions belong to the declarations.
    while next.progress().image.written_units == 0 {
        assert_eq!(next.advance(1, 1024).unwrap(), BackupCaptureStep::Pending);
    }
    let inspected = next.progress().source;
    assert!(inspected.visited_positions > 0, "{inspected:?}");
    if kind.is_some() {
        assert!(inspected.metadata_read_bytes > 0, "{inspected:?}");
    }
    let (changed, report) = capture_report(next);
    let (independent, _) = capture_report(start(None));
    if kind.is_some() {
        sql(&engine, "ROLLBACK");
    }
    assert_eq!(calls.load(Ordering::SeqCst), before);
    assert_eq!(independent.summary(), changed.summary());
    assert_ne!(changed.summary(), parent.summary());
    let read = |root: BackupRoot| {
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        lease.verify(16 << 20, &control).unwrap();
        let mut declarations = lease.function_references(&control).unwrap().unwrap();
        let mut kinds = Vec::new();
        loop {
            match declarations.advance(1).unwrap() {
                BackupSourceStep::Unit => {
                    let reference = declarations.current().unwrap();
                    assert_eq!(reference.name, "abs");
                    kinds.push(reference.kind);
                }
                BackupSourceStep::Pending => (),
                BackupSourceStep::Complete => return kinds,
            }
        }
    };
    assert_eq!(read(steady), [Scalar, AbsentTable, AbsentAggregate]);
    assert_eq!(read(changed), [AbsentScalar, AbsentTable, AbsentAggregate]);
    assert_eq!(report.image.written_units, 1);
    let bytes = function_section_bytes(&repository, changed, &control);
    assert_eq!(report.image.written_payload_bytes, bytes);
    assert!(report.image.hashed_payload_bytes >= bytes);
    assert!(report.image.certified_reused_bytes >= 65536, "{report:?}");
    assert_eq!(
        report.image.hashed_payload_bytes + report.image.certified_reused_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn public_backup_admission_preserves_authority_and_failed_transaction_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("database")).unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let path = directory.path().join("repository");
    let repository = repository(&path, &control, false);
    let options = BackupOptions::new(16 << 20, 16 << 20);
    sql(&engine, "CREATE ROLE restricted");
    sql(&engine, "SET ROLE restricted");
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("42501"));
    sql(&engine, "RESET ROLE");
    sql(&engine, "BEGIN");
    assert!(engine.sql("SELECT 1 / 0", &[]).is_err());
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("25P02"));
    assert_eq!(
        std::fs::read_dir(path.join("operations")).unwrap().count(),
        0
    );
    sql(&engine, "ROLLBACK");
    let denied = StorageReadControl::with_limit(0);
    let error = engine
        .begin_backup(&repository, options, &denied)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(denied.memory().used(), 0);
    assert_eq!(engine.transaction_depth(), 0);
    let mut job = engine.begin_backup(&repository, options, &control).unwrap();
    assert!(job.cancel().unwrap().is_none());
    drop(job);
    assert_eq!(control.memory().used(), 0);

    let memory = Engine::new();
    assert_eq!(
        memory.backup_capabilities(),
        BackupCapabilities {
            export: true,
            import: true,
            snapshot_fingerprints: true
        }
    );
    sql(&memory, "CREATE ROLE restricted; SET ROLE restricted");
    assert_eq!(
        memory
            .begin_backup(&repository, options, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("42501")
    );
    sql(&memory, "RESET ROLE");
    sql(&memory, "BEGIN");
    assert!(memory.sql("SELECT 1 / 0", &[]).is_err());
    assert_eq!(
        memory
            .begin_backup(&repository, options, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("25P02")
    );
    sql(&memory, "ROLLBACK");
    let mut job = memory.begin_backup(&repository, options, &control).unwrap();
    assert!(job.cancel().unwrap().is_none());
    drop(job);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::plain(false, false)]
#[case::protected(true, false)]
#[case::plain_changed(false, true)]
#[case::protected_changed(true, true)]
fn public_memory_backup_publishes_full_and_independent_incremental_roots(
    #[case] encrypted: bool,
    #[case] committed_change: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE saved(id integer PRIMARY KEY, body text); INSERT INTO saved VALUES(1,'original'),(2,'retained')");
    let control = StorageReadControl::with_limit(32 << 20);
    let repository = repository(&directory.path().join("repository"), &control, encrypted);
    let options = BackupOptions::new(16 << 20, 16 << 20);
    let mut full = engine.begin_backup(&repository, options, &control).unwrap();
    while full.advance(1, 127).unwrap() == BackupCaptureStep::Pending {}
    let full_report = full.report().unwrap();
    assert!(full_report.image.written_units > 25);
    assert_eq!(full_report.image.reused_units, 0);
    let root = full.publish().unwrap();
    drop(full);
    assert_eq!(control.memory().used(), 0);
    if committed_change {
        sql(&engine, "UPDATE saved SET body='committed' WHERE id=1");
    }
    sql(&engine, "BEGIN; UPDATE saved SET body='private'");
    let mut child = engine
        .begin_backup(
            &repository,
            BackupOptions {
                parent: Some(root.backup()),
                ..options
            },
            &control,
        )
        .unwrap();
    assert_eq!(child.cut().format.provider(), ImageProvider::Memory);
    assert_eq!(engine.transaction_depth(), 1);
    sql(
        &engine,
        "ROLLBACK; UPDATE saved SET body='later'; DROP TABLE saved",
    );
    drop(engine);
    while child.advance(1, 113).unwrap() == BackupCaptureStep::Pending {}
    let report = child.report().unwrap();
    if committed_change {
        assert!(report.image.written_units > 0);
        assert!(report.image.reused_units > 0);
        assert_eq!(
            report.image.written_units + report.image.reused_units,
            full_report.image.written_units
        );
    } else {
        assert_eq!(report.image.written_units, 0);
        assert_eq!(report.image.reused_units, full_report.image.written_units);
    }
    let child_root = child.publish().unwrap();
    assert_eq!(child.publish().unwrap(), child_root);
    drop(child);
    repository.remove_backup(root.backup(), &control).unwrap();
    let lease = repository
        .open_backup(child_root.backup(), &control)
        .unwrap();
    let mut input = MemoryImageImportReader::new(lease, 16 << 20, &control).unwrap();
    let mut catalogs = 0;
    let mut documents = std::collections::BTreeMap::new();
    loop {
        match input.advance(113).unwrap() {
            BackupSourceStep::Complete => break,
            BackupSourceStep::Pending => continue,
            BackupSourceStep::Unit => (),
        }
        let document = match input.current_unit().unwrap().address {
            DecodedMemoryImageAddress::Catalog(_) => {
                catalogs += 1;
                false
            }
            DecodedMemoryImageAddress::Table {
                schema,
                relation,
                unit: DecodedMemoryTableUnit::Document(_),
                ..
            } => {
                assert_eq!(&**schema, "public");
                assert_eq!(&**relation, "saved");
                true
            }
            _ => false,
        };
        let mut bytes = Vec::new();
        let mut buffer = [0; 127];
        loop {
            let read = input.read_current(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        if document {
            let decoded =
                uqa_storage::document_store::image::decode_document_image(&bytes, &control)
                    .unwrap();
            let Value::Int(id) = &decoded.fields()["id"] else {
                panic!("restored integer identity")
            };
            assert!(documents
                .insert(*id, decoded.fields()["body"].clone())
                .is_none());
        }
    }
    assert_eq!(catalogs, 24);
    assert_eq!(input.header().unwrap().source, child_root.cut().source);
    assert_eq!(documents.len(), 2);
    assert_eq!(
        documents[&1],
        Value::Str(
            if committed_change {
                "committed"
            } else {
                "original"
            }
            .into()
        )
    );
    assert_eq!(documents[&2], Value::Str("retained".into()));
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::native(Provider::Native)]
#[case::kv(Provider::KV)]
#[case::redb(Provider::Redb)]
fn public_backup_repository_admission_failure_preserves_the_source_transaction(
    #[case] kind: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let provider = kind.open(&directory.path().join("database"), 0);
    let engine = Engine::from_persistent_provider(provider).unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, false);
    sql(&engine, "CREATE TABLE values_to_keep (value INTEGER)");
    sql(&engine, "BEGIN");
    sql(&engine, "INSERT INTO values_to_keep VALUES (42)");
    let error = engine
        .begin_backup(&repository, BackupOptions::new(1 << 20, 1), &control)
        .err()
        .unwrap();
    assert!(
        matches!(error, uqa_sql::SQLError::Host(ref source) if matches!(source.downcast_ref::<BackupError>(), Some(BackupError::Limit { resource: "capture workspace", .. })))
    );
    assert_eq!(engine.transaction_depth(), 1);
    assert_eq!(
        sql(&engine, "SELECT value FROM values_to_keep")[0]["value"],
        Value::Int(42)
    );
    sql(&engine, "COMMIT");
    assert_eq!(
        sql(&engine, "SELECT value FROM values_to_keep")[0]["value"],
        Value::Int(42)
    );
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::native_encrypted(Provider::Native, 1)]
#[case::native_compressed_encrypted(Provider::Native, 3)]
#[case::kv_encrypted(Provider::KV, 1)]
#[case::kv_compressed_encrypted(Provider::KV, 3)]
fn public_backup_requires_explicit_plaintext_export(#[case] kind: Provider, #[case] mode: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine =
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), mode))
            .unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let path = directory.path().join("repository");
    let repository = repository(&path, &control, false);
    assert!(repository.protection_domain().is_none());
    let options = BackupOptions::new(16 << 20, 16 << 20);
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        uqa_sql::SQLError::Host(ref source)
            if matches!(source.downcast_ref::<BackupError>(), Some(BackupError::Invalid(_)))
    ));
    assert_eq!(
        std::fs::read_dir(path.join("operations")).unwrap().count(),
        0
    );
    let job = engine
        .begin_backup(
            &repository,
            BackupOptions {
                allow_plaintext_export: true,
                ..options
            },
            &control,
        )
        .unwrap();
    drop(engine);
    let root = finish(job, None);
    repository
        .open_backup(root.backup(), &control)
        .unwrap()
        .verify(16 << 20, &control)
        .unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn public_backup_retains_job_cancellation_after_engine_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("source")).unwrap();
    sql(&engine, "CREATE TABLE rows (id INTEGER)");
    sql(&engine, "INSERT INTO rows VALUES (7)");
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, false);
    let mut job = engine
        .begin_backup(
            &repository,
            BackupOptions::new(16 << 20, 16 << 20),
            &control,
        )
        .unwrap();
    drop(engine);
    assert_eq!(job.advance(1, 1).unwrap(), BackupCaptureStep::Pending);
    control.cancellation().cancel();
    assert!(matches!(job.advance(1, 1), Err(BackupError::Cancelled(_))));
    assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
    assert!(job.advance(1, 1).is_err());
    assert!(job.publish().is_err());
    drop(job);
    assert_eq!(control.memory().used(), 0);
}
