//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::binary_path;
use std::fs;
use std::process::{Command, Output, Stdio};

#[path = "protection.rs"]
mod protection;
#[path = "recovery.rs"]
mod recovery;
#[path = "shell.rs"]
mod shell;

const SEED: &str = "CREATE TABLE items(id INT PRIMARY KEY, note TEXT); INSERT INTO items VALUES (1, 'old'), (2, 'untouched'), (3, 'deleted');";
const CHANGE: &str = "UPDATE items SET note = 'latest' WHERE id = 1; DELETE FROM items WHERE id = 3; INSERT INTO items VALUES (4, 'new');";
const ROWS: &str = "SELECT id, note FROM items ORDER BY id;";
const WRITE: &str = "INSERT INTO items VALUES (5, 'later');";
const SNAPSHOT: &str = "1\tlatest\n2\tuntouched\n4\tnew\n";
const WRITTEN: &str = "1\tlatest\n2\tuntouched\n4\tnew\n5\tlater\n";

struct Cli(tempfile::TempDir);

impl Cli {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn command(&self, connection: &[&str]) -> Command {
        let mut command = Command::new(binary_path());
        command
            .current_dir(self.0.path())
            .env("UQA_HISTORY", self.0.path().join("history"))
            .env_remove("UQA_KEY")
            .args(connection);
        command
    }

    fn backup(&self, connection: &[&str], arguments: &[&str]) -> Output {
        self.command(connection)
            .arg("backup")
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn sql(&self, connection: &[&str], sql: &str) -> String {
        success(
            self.command(connection)
                .args(["--copy-text", "-c", sql])
                .stdin(Stdio::null())
                .output()
                .unwrap(),
        )
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "usql exited {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "\n")
}

fn backup_id(output: Output) -> String {
    let stdout = success(output);
    let id = stdout.strip_suffix('\n').expect("newline after backup ID");
    assert!(is_id(id), "unexpected backup ID output {stdout:?}");
    id.to_owned()
}

fn is_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn retire_parent(cli: &Cli, repo: &str, full: &str, incremental: &str) {
    let listing = success(cli.backup(&[], &["list", repo]));
    let mut entries: Vec<_> = listing.lines().collect();
    entries.sort_unstable();
    let mut expected = [format!("{full}\t-"), format!("{incremental}\t{full}")];
    expected.sort_unstable();
    assert_eq!(entries, expected);
    success(cli.backup(&[], &["remove", repo, full]));
    let collected = success(cli.backup(&[], &["gc", repo, "--max-files", "4096"]));
    assert!(collected.ends_with(" remaining_files=0\n"), "{collected}");
    assert!(!cli.backup(&[], &["verify", repo, full]).status.success());
    success(cli.backup(&[], &["verify", repo, incremental]));
    assert_eq!(
        success(cli.backup(&[], &["list", repo])),
        format!("{incremental}\t{full}\n")
    );
}

fn persistent_incremental(provider: &str) {
    let cli = Cli::new();
    fs::create_dir(cli.0.path().join("original")).unwrap();
    let source = ["--db", "original/source file", "--storage", provider];
    let restored = ["--db", "restored directory/database", "--storage", provider];
    let repo = "archive directory";
    success(cli.backup(&[], &["init", repo]));
    let (full, incremental) = if provider == "redb" {
        shell::capture_in_session(&cli, &source, repo)
    } else {
        cli.sql(&source, SEED);
        let full = backup_id(cli.backup(&source, &["create", repo]));
        cli.sql(&source, CHANGE);
        let incremental = backup_id(cli.backup(&source, &["create", repo, "--parent", &full]));
        (full, incremental)
    };
    assert_ne!(incremental, full);
    fs::remove_dir_all(cli.0.path().join("original")).unwrap();
    retire_parent(&cli, repo, &full, &incremental);

    let restore = [
        "restore",
        repo,
        &incremental,
        "restored directory",
        "--target-storage",
        provider,
    ];
    success(cli.backup(&[], &restore));
    assert_eq!(cli.sql(&restored, ROWS), SNAPSHOT);
    cli.sql(&restored, WRITE);
    assert!(!cli.backup(&[], &restore).status.success());
    assert_eq!(cli.sql(&restored, ROWS), WRITTEN);
}

#[test]
fn sqlite_native_incremental_survives_parent_retirement_and_cold_reopen() {
    persistent_incremental("sqlite-native");
}

#[test]
fn sqlite_kv_incremental_survives_parent_retirement_and_cold_reopen() {
    persistent_incremental("sqlite-kv");
}

#[test]
fn redb_incremental_survives_parent_retirement_and_cold_reopen() {
    persistent_incremental("redb");
}

#[test]
fn invalid_backup_arguments_and_missing_source_leave_the_repository_empty() {
    let cli = Cli::new();
    success(cli.backup(&[], &["init", "repo"]));
    let missing = cli.backup(&["--db", "missing.db"], &["create", "repo"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("existing source database"));
    assert!(!cli.0.path().join("missing.db").exists());
    for (arguments, diagnostic) in [
        (&["verify", "repo"][..], "backup ID"),
        (&["verify", "repo", "123"][..], "32-digit hexadecimal"),
        (
            &["gc", "repo", "--max-files", "1", "--max-files", "2"][..],
            "duplicate --max-files",
        ),
        (
            &["create", "repo", "--allow-plaintext", "--allow-plaintext"][..],
            "duplicate --allow-plaintext",
        ),
    ] {
        let rejected = cli.backup(&[], arguments);
        assert!(!rejected.status.success(), "{arguments:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(diagnostic),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );
    }
    assert_eq!(success(cli.backup(&[], &["list", "repo"])), "");
}
