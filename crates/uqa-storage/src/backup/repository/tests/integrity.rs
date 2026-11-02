//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Seek, SeekFrom, Write};

use super::*;

#[test]
fn full_verification_rejects_corrupted_and_missing_data_objects() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    prepare(&repository, &mut operation, &control);
    let root = operation.publish(&control).unwrap();
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    for entry in repository.objects.entries().unwrap() {
        let entry = entry.unwrap();
        if entry.file_name()
            == directory::text(&directory::name(root.inventory().object().as_bytes()))
        {
            continue;
        }
        let mut file = directory::open_file(&repository.objects, entry.file_name(), true).unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        file.write_all(b"corrupt").unwrap();
        assert!(matches!(
            lease.verify(1024 * 1024, &control),
            Err(BackupError::Integrity)
        ));
        repository.objects.remove_file(entry.file_name()).unwrap();
        assert!(lease.verify(1024 * 1024, &control).is_err());
    }
}

#[test]
fn verified_span_requires_whole_object_digest_and_exact_frame_extent() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let span = operation
        .write_object(FrameKind::Unit, &mut Cursor::new([7; 8192]), &control)
        .unwrap();
    let wrong = ObjectSpan::new(
        span.object(),
        ImageDigest::of_bytes(b"wrong"),
        span.object_bytes(),
        span.offset(),
        span.encoded_bytes(),
        span.frames(),
    )
    .unwrap();
    assert!(matches!(
        repository.span_reader(wrong, FrameKind::Unit, true, &control),
        Err(BackupError::Integrity)
    ));
    let short = ObjectSpan::new(
        span.object(),
        span.digest(),
        span.object_bytes(),
        span.offset(),
        span.encoded_bytes() - 1,
        span.frames(),
    )
    .unwrap();
    let mut reader = repository
        .span_reader(short, FrameKind::Unit, true, &control)
        .unwrap();
    assert!(std::io::copy(&mut reader, &mut std::io::sink()).is_err());
    assert!(reader.read(&mut [0]).is_err());
}

#[cfg(unix)]
#[test]
fn repositories_reject_internal_symlinks_and_pin_selected_directory_identity() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let moved = directory.path().join("moved");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    prepare(&repository, &mut operation, &control);
    let root = operation.publish(&control).unwrap();
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    let reopened = LocalRepository::open(&moved, protected(), limits(), &control).unwrap();
    reopened
        .open_backup(root.backup(), &control)
        .unwrap()
        .verify(1024 * 1024, &control)
        .unwrap();
    drop(reopened);
    std::fs::rename(moved.join("objects"), directory.path().join("objects")).unwrap();
    symlink(directory.path().join("objects"), moved.join("objects")).unwrap();
    assert!(LocalRepository::open(&moved, protected(), limits(), &control).is_err());
}

#[cfg(unix)]
#[test]
fn a_substituted_pipe_is_rejected_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    std::fs::remove_file(path.join("generation")).unwrap();
    let output = std::process::Command::new("mkfifo")
        .arg(path.join("generation"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(matches!(
        repository.generation(&control),
        Err(BackupError::Invalid(
            "repository entry is not a regular file"
        ))
    ));
}
