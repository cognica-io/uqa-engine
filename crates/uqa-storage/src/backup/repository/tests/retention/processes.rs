//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn independent_process_leases_protect_readers_writers_and_prepared_recovery() {
    const CHILD: &str = "UQA_BACKUP_RETENTION_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        let control = control();
        let repository = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
        let mode = std::env::var("UQA_BACKUP_RETENTION_MODE").unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1 << 20, &control)
            .unwrap();
        if mode == "writer" {
            operation
                .write_object(FrameKind::Unit, &mut Cursor::new(b"pending"), &control)
                .unwrap();
        } else {
            prepare(&repository, &mut operation, &control);
        }
        let root = if mode == "reader" {
            Some(operation.publish(&control).unwrap())
        } else {
            None
        };
        let _reader = root.map(|root| repository.open_backup(root.backup(), &control).unwrap());
        let ready = std::path::Path::new(&path).with_extension("ready");
        let pending_ready = ready.with_extension("pending");
        std::fs::write(&pending_ready, operation.id().as_bytes()).unwrap();
        std::fs::rename(pending_ready, ready).unwrap();
        std::io::stdin().read_exact(&mut [0; 1]).unwrap();
        std::process::exit(0);
    }
    for mode in ["reader", "writer", "prepared"] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("repository");
        let control = control();
        let repository = repository(&path, &control);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "backup::repository::tests::retention::processes::independent_process_leases_protect_readers_writers_and_prepared_recovery", "--nocapture"])
            .env(CHILD, &path).env("UQA_BACKUP_RETENTION_MODE", mode)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        let ready = path.with_extension("ready");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before acquiring its lease"
            );
            assert!(Instant::now() < deadline, "child did not acquire its lease");
            std::thread::sleep(Duration::from_millis(10));
        }
        let id = BackupId::from_bytes(std::fs::read(ready).unwrap().try_into().unwrap()).unwrap();
        assert!(matches!(
            repository.remove_backup(id, &control),
            Err(BackupError::Busy)
        ));
        let before = objects(&repository);
        repository.collect_garbage(100, 1 << 20, &control).unwrap();
        assert_eq!(objects(&repository), before);
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        collect_all(&repository, &control);
        if mode == "writer" {
            assert_eq!(objects(&repository), (0, 0));
        } else {
            let mut publication = repository
                .resume_publication(id, 1 << 20, &control)
                .unwrap();
            let root = publication.publish(&control).unwrap();
            repository
                .open_backup(root.backup(), &control)
                .unwrap()
                .verify(1 << 20, &control)
                .unwrap();
        }
    }
}
