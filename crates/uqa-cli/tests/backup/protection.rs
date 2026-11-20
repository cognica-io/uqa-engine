//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{backup_id, success, Cli, ROWS, SEED};
use std::fs;

#[test]
fn protected_source_and_repository_keys_are_independent_of_the_target_database_key() {
    let cli = Cli::new();
    let path = cli.0.path();
    fs::write(path.join("source.key"), "source passphrase\n").unwrap();
    fs::write(path.join("destination.key"), "destination passphrase\n").unwrap();
    fs::write(path.join("archive.key"), [29u8; 32]).unwrap();
    fs::write(path.join("copy.key"), [197u8; 32]).unwrap();
    let source = ["--db", "source.db", "--key-file", "source.key"];
    cli.sql(&source, SEED);
    success(cli.backup(&[], &["init", "plaintext"]));
    assert!(!cli
        .backup(&source, &["create", "plaintext"])
        .status
        .success());
    assert_eq!(success(cli.backup(&[], &["list", "plaintext"])), "");
    let capture = |maximum| {
        cli.backup(
            &source,
            &[
                "create",
                "plaintext",
                "--allow-plaintext",
                "--max-encoded-bytes",
                maximum,
            ],
        )
    };
    let too_small = capture("1");
    assert!(!too_small.status.success());
    assert_eq!(success(cli.backup(&[], &["list", "plaintext"])), "");
    let plain = backup_id(capture("16777216"));
    success(cli.backup(&[], &["verify", "plaintext", &plain]));

    let archive_domain = "1234567890abcdef1234567890abcdef";
    let copy_domain = "cdef1234567890abcdef1234567890ab";
    let credentials = |domain, key| ["--domain", domain, "--repository-key-file", key];
    let archive = credentials(archive_domain, "archive.key");
    let copy = credentials(copy_domain, "copy.key");
    let secured = |connection: &[&str], command: &[&str], protection: &[&str]| {
        cli.backup(connection, &[command, protection].concat())
    };
    success(secured(&[], &["init", "vault"], &archive));
    success(secured(&[], &["init", "copy"], &copy));
    let id = backup_id(secured(&source, &["create", "vault"], &archive));
    let wrong = credentials(archive_domain, "copy.key");
    assert!(!secured(&[], &["list", "vault"], &wrong).status.success());
    let copied = backup_id(secured(
        &[],
        &[
            "copy",
            "vault",
            &id,
            "copy",
            "--destination-domain",
            copy_domain,
            "--destination-key-file",
            "copy.key",
        ],
        &archive,
    ));
    success(secured(&[], &["remove", "vault", &id], &archive));
    success(secured(&[], &["gc", "vault"], &archive));
    success(secured(&[], &["verify", "copy", &copied], &copy));
    success(secured(
        &[],
        &[
            "restore",
            "copy",
            &copied,
            "restored",
            "--target-storage",
            "sqlite-native",
            "--target-key-file",
            "destination.key",
            "--target-compression",
            "zstd",
        ],
        &copy,
    ));
    let target = ["--db", "restored/database", "--key-file", "destination.key"];
    assert_eq!(cli.sql(&target, ROWS), "1\told\n2\tuntouched\n3\tdeleted\n");
    let wrong_key = cli
        .command(&["--db", "restored/database", "--key-file", "source.key"])
        .args(["-c", "SELECT 1"])
        .output()
        .unwrap();
    assert!(!wrong_key.status.success());
}
