//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn unchanged_source_reuses_its_identity_but_replacement_fails_on_the_next_call() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let connection = Connection::open(&path).unwrap();
    let mut spec = ConnectionSpec::File {
        path: path.clone(),
        key: None,
    };
    let source = DatabaseSource::capture(&mut spec, &connection)
        .unwrap()
        .unwrap();
    let watched = source.watch.lock().is_some();
    let before = source.identity_reads.load(Ordering::Relaxed);
    for _ in 0..256 {
        source.check().unwrap();
    }
    let reads = source.identity_reads.load(Ordering::Relaxed) - before;
    eprintln!("256 source checks: watch={watched}, identity reads={reads}");
    assert_eq!(reads, if watched { 0 } else { 256 });
    drop(connection);
    let saved = path.with_extension("saved");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(matches!(
        source.check(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    assert!(matches!(
        source.check(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
}

#[test]
fn direct_source_checks_preserve_replacement_detection_without_a_watch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let connection = Connection::open(&path).unwrap();
    let mut spec = ConnectionSpec::File {
        path: path.clone(),
        key: None,
    };
    let source = DatabaseSource::capture(&mut spec, &connection)
        .unwrap()
        .unwrap();
    *source.watch.lock() = None;
    let before = source.identity_reads.load(Ordering::Relaxed);
    for _ in 0..8 {
        source.check().unwrap();
    }
    assert_eq!(source.identity_reads.load(Ordering::Relaxed) - before, 8);
    drop(connection);
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        source.check(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
}

#[test]
fn source_rejects_replacement_of_an_ancestor_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("original/nested")).unwrap();
    let path = root.join("original/nested/source.db");
    let connection = Connection::open(&path).unwrap();
    let mut spec = ConnectionSpec::File {
        path: path.clone(),
        key: None,
    };
    let source = DatabaseSource::capture(&mut spec, &connection)
        .unwrap()
        .unwrap();
    source.check().unwrap();
    drop(connection);
    std::fs::rename(root.join("original"), root.join("saved")).unwrap();
    std::fs::create_dir_all(root.join("original/nested")).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(matches!(
        source.check(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
}

#[cfg(unix)]
#[test]
fn failed_path_resolution_does_not_authorize_reuse_of_an_unchecked_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let connection = Connection::open(&path).unwrap();
    let mut spec = ConnectionSpec::File {
        path: path.clone(),
        key: None,
    };
    let source = DatabaseSource::capture(&mut spec, &connection)
        .unwrap()
        .unwrap();
    drop(connection);
    let saved = path.with_extension("saved");
    std::fs::rename(&path, &saved).unwrap();
    std::os::unix::fs::symlink(&path, &path).unwrap();
    assert!(source.check().is_err());
    assert!(source.check().is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    source.check().unwrap();
}
