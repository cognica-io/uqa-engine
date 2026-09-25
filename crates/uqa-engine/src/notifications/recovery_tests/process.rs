//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An idle surviving process recovers a sender that exits without publishing a wake signal.

use super::{open, Duration, Instant, Path, Value, KEY};
use std::process::Command;

const CHILD_PATH: &str = "UQA_NOTIFICATION_LOSS_DATABASE";

fn assert_encrypted_files(path: &Path) {
    let mut inspected = 0;
    for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_file() {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read encrypted recovery fixture: {error}"),
        };
        inspected += 1;
        for marker in [KEY, "committed_items", "process original"] {
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|value| value == marker.as_bytes()),
                "plaintext recovery state in {}",
                entry.path().display()
            );
        }
    }
    assert!(inspected >= 2, "inspect both main and registry files");
}

#[test]
fn notification_sender_process() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let provider = std::env::var("UQA_NOTIFICATION_LOSS_PROVIDER")
        .unwrap()
        .parse()
        .unwrap();
    let sender = open(provider, Path::new(&path));
    sender
        .sql(
            "BEGIN; INSERT INTO items VALUES(1); NOTIFY committed_items, 'process original'",
            &[],
        )
        .unwrap();
    let stack = sender.session.transactions.lock();
    let mut guard = sender
        .begin_notification_commit(true, stack.last().unwrap())
        .unwrap()
        .unwrap();
    sender
        .storage
        .backend
        .as_ref()
        .unwrap()
        .commit_transaction()
        .unwrap();
    if std::env::var("UQA_NOTIFICATION_LOSS_REGISTRY").unwrap() == "committed" {
        guard
            .cross
            .as_mut()
            .unwrap()
            .registry
            .take()
            .unwrap()
            .commit()
            .unwrap();
    }
    if provider >= 4 {
        assert_encrypted_files(Path::new(&path));
    }
    let preparing = Path::new(&path).with_extension("sender-pid-preparing");
    std::fs::write(&preparing, sender.backend_process_id().to_string()).unwrap();
    std::fs::rename(preparing, Path::new(&path).with_extension("sender-pid")).unwrap();
    // The parent kills this prepared child, bypassing Rust destructors and C exit handlers while the registry and session resources remain unfinished.
    loop {
        std::thread::park();
    }
}

#[rstest::rstest]
fn idle_listener_recovers_an_exited_process_without_a_sender_wakeup(
    // redb holds an exclusive process lock on its main file; its supported independent-session recovery is covered in the parent module.
    #[values(0, 1, 3, 4, 5, 6, 7)] provider: usize,
    #[values(false, true)] registry_committed: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("process.db");
    let listener = open(provider, &path);
    listener
        .sql(
            "CREATE TABLE items(id INTEGER); LISTEN committed_items",
            &[],
        )
        .unwrap();
    let mut pending = listener.runtime.notifications.lock();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "notifications::recovery_tests::process::notification_sender_process",
            "--nocapture",
        ])
        .env(CHILD_PATH, &path)
        .env("UQA_NOTIFICATION_LOSS_PROVIDER", provider.to_string())
        .env(
            "UQA_NOTIFICATION_LOSS_REGISTRY",
            if registry_committed {
                "committed"
            } else {
                "pending"
            },
        )
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let ready = path.with_extension("sender-pid");
    while !ready.is_file() && Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(
        ready.is_file(),
        "sender did not prepare process loss: {status}"
    );
    assert!(!status.success(), "sender exited normally: {status}");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    }
    while pending.is_empty() && Instant::now() < deadline {
        // This is the listener's existing condition variable, not polling SQL or the registry. Only the surviving recovery worker can complete the delivery.
        listener.runtime.notification_wake.wait_for(
            &mut pending,
            deadline.saturating_duration_since(Instant::now()),
        );
    }
    assert_eq!(
        pending.len(),
        1,
        "idle recovery did not wake the surviving listener"
    );
    let original = pending.pop_front().unwrap();
    drop(pending);
    let process_id: i32 = std::fs::read_to_string(path.with_extension("sender-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(original.process_id, process_id);
    assert_eq!(original.payload, "process original");
    assert_eq!(
        listener
            .sql("SELECT id FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    listener.poll_sql_notifications().unwrap();
    assert!(listener.take_sql_notifications().is_empty());
    if provider >= 4 {
        assert_encrypted_files(&path);
    }
}
