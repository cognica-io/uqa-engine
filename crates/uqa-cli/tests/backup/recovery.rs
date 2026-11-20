//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{backup_id, success, Cli, ROWS, SEED};
use uqa_engine::{
    backup::{
        BackupCaptureStep, BackupLimits, BackupOptions, LocalRepository, Protection,
        RestoreDestination, RestoreOptions, RestorePhase, SQLiteBackupMapping, SQLiteBackupTarget,
        StorageReadControl,
    },
    Engine,
};

#[test]
fn recovery_commands_adopt_prepared_requests_after_the_original_handles_close() {
    let cli = Cli::new();
    success(cli.backup(&[], &["init", "repo"]));
    let control = StorageReadControl::with_limit(64 << 20);
    let repository = LocalRepository::open(
        cli.0.path().join("repo"),
        Protection::Plaintext,
        BackupLimits::default(),
        &control,
    )
    .unwrap();
    let source = Engine::open(&cli.0.path().join("source.db")).unwrap();
    source.sql(SEED, &[]).unwrap();
    let mut job = source
        .begin_backup(&repository, BackupOptions::default(), &control)
        .unwrap();
    while job.advance(128, 65536).unwrap() == BackupCaptureStep::Pending {}
    let backup = job.id();
    let id = format!("{:032x}", u128::from_be_bytes(backup.as_bytes()));
    drop((job, source));
    assert_eq!(success(cli.backup(&[], &["list", "repo"])), "");
    assert_eq!(
        backup_id(cli.backup(&[], &["resume-publication", "repo", &id])),
        id
    );
    let prepare = |directory| {
        let authority = Engine::new();
        let target = RestoreDestination::persistent(
            cli.0.path().join(directory),
            SQLiteBackupTarget::new(SQLiteBackupMapping::Native),
        );
        let mut request = authority
            .begin_restore(
                &repository,
                backup,
                target,
                RestoreOptions::default(),
                &control,
            )
            .unwrap();
        while matches!(
            request.phase(),
            RestorePhase::Reading | RestorePhase::Validating
        ) {
            request.advance(65536).unwrap();
        }
        assert_eq!(request.phase(), RestorePhase::Prepared);
        format!(
            "{:032x}",
            u128::from_be_bytes(request.id().unwrap().as_bytes())
        )
    };
    let cancelled = prepare("cancelled");
    let resumed = prepare("resumed");
    drop(repository);
    success(cli.backup(&[], &["remove", "repo", &id]));
    success(cli.backup(&[], &["gc", "repo"]));
    success(cli.backup(&[], &["cancel-restore", "repo", &cancelled, "cancelled"]));
    assert!(!cli.0.path().join("cancelled").exists());
    assert!(!cli
        .backup(&[], &["resume-restore", "repo", &cancelled, "cancelled"])
        .status
        .success());
    let restored = cli.backup(&[], &["resume-restore", "repo", &resumed, "resumed"]);
    assert!(String::from_utf8_lossy(&restored.stderr).contains(&resumed));
    success(restored);
    assert_eq!(
        cli.sql(&["--db", "resumed/database"], ROWS),
        "1\told\n2\tuntouched\n3\tdeleted\n"
    );
}
