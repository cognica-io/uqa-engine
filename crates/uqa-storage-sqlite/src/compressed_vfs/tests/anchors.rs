//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn live_anchor_rejects_rollback_and_forks_when_opening_another_physical_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live-anchor.uqac.sqlite3");
    let fork_path = dir.path().join("fork.uqac.sqlite3");
    let mut options = encrypted_options();
    let mut writer = ContainerFile::open(path.clone(), options.clone()).unwrap();
    writer.write_at(0, &[b'a'; 512]).unwrap();
    writer.flush().unwrap();
    let original = fs::read(&path).unwrap();
    fs::write(&fork_path, &original).unwrap();
    *options.live_anchor.lock().unwrap() = Some(writer.authenticated_anchor());
    options.trusted_anchor = Some(writer.authenticated_anchor());
    writer.live_anchor = Some(options.live_anchor.clone());
    writer.write_at(0, b"local commit").unwrap();
    writer.flush().unwrap();
    let current = fs::read(&path).unwrap();
    let flags = ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_READWRITE;
    assert!(VfsFile::open(path.clone(), options.clone(), flags, false, OpenScope::Pool).is_ok());
    // A new registration cannot adopt a commit that raced its original exact anchor.
    assert!(VfsFile::open(
        path.clone(),
        options.clone(),
        flags,
        false,
        OpenScope::Registration
    )
    .is_err());
    fs::write(&path, original).unwrap();
    assert!(VfsFile::open(path.clone(), options.clone(), flags, false, OpenScope::Pool).is_err());

    let mut fork = ContainerFile::open(fork_path.clone(), options.clone()).unwrap();
    fork.write_at(0, b"forked state").unwrap();
    fork.flush().unwrap();
    assert_eq!(fork.generation, writer.generation);
    assert_ne!(fork.state_tag, writer.state_tag);
    fs::copy(&fork_path, &path).unwrap();
    assert!(VfsFile::open(path.clone(), options.clone(), flags, false, OpenScope::Pool).is_err());
    fs::write(&path, current).unwrap();
    assert!(VfsFile::open(path, options, flags, false, OpenScope::Pool).is_ok());
}

#[test]
fn trusted_anchor_rejects_a_divergent_same_generation_fork() {
    let dir = tempfile::tempdir().unwrap();
    let original_path = dir.path().join("anchor-original.uqac.sqlite3");
    let fork_path = dir.path().join("anchor-fork.uqac.sqlite3");
    let options = encrypted_options();

    let mut base = ContainerFile::open(original_path.clone(), options.clone()).unwrap();
    base.write_at(0, &[b'a'; 512]).unwrap();
    base.flush().unwrap();
    drop(base);
    std::fs::copy(&original_path, &fork_path).unwrap();

    let mut original = ContainerFile::open(original_path, options.clone()).unwrap();
    original.write_at(0, &[b'b'; 512]).unwrap();
    original.flush().unwrap();
    let trusted = original.authenticated_anchor();

    let mut fork = ContainerFile::open(fork_path, options).unwrap();
    fork.write_at(0, &[b'c'; 512]).unwrap();
    fork.flush().unwrap();
    let divergent = fork.authenticated_anchor();
    assert_eq!(trusted.database_id, divergent.database_id);
    assert_eq!(trusted.generation, divergent.generation);
    assert_ne!(trusted.state_tag, divergent.state_tag);
    let error = fork.require_trusted_anchor(trusted).unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match the trusted anchor"));
}
