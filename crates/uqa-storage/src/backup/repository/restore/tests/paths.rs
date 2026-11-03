//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn restore_provider_path_requires_regular_installed_names_and_retires_memory() {
    let temporary = tempfile::tempdir().unwrap();
    let pinned = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    let directory = RestoreDirectory { directory: &pinned };
    drop(directory.create_file("database").unwrap());
    pinned.create_dir("nested").unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    for name in [
        "",
        ".",
        "..",
        "../database",
        "nested/database",
        "/database",
        "nul\0byte",
        MARKER,
        "nested",
        "missing",
    ] {
        assert!(directory
            .with_file_path(name, &control, |_| panic!("invalid path reached provider"))
            .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    #[cfg(unix)]
    {
        pinned.symlink("database", "link").unwrap();
        assert!(directory
            .with_file_path("link", &control, |_| panic!("symlink reached provider"))
            .is_err());
    }
    directory
        .with_file_path("database", &control, |path| {
            assert!(path.is_absolute());
            assert!(control.memory().used() >= path.as_os_str().len());
            std::fs::write(path, b"provider data")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(pinned.read("database").unwrap(), b"provider data");
    assert_eq!(control.memory().used(), 0);
    let low = StorageReadControl::with_limit(1);
    assert!(directory
        .with_file_path("database", &low, |_| panic!("memory admission failed"))
        .is_err());
    assert_eq!(low.memory().used(), 0);
    control.cancellation().cancel();
    assert!(directory
        .with_file_path("database", &control, |_| panic!("cancelled provider call"))
        .is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn restore_provider_path_keeps_errors_and_rejects_nonregular_completion() {
    let temporary = tempfile::tempdir().unwrap();
    let pinned = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    let directory = RestoreDirectory { directory: &pinned };
    drop(directory.create_file("database").unwrap());
    let control = StorageReadControl::with_limit(128 << 10);
    let result = directory.with_file_path("database", &control, |_| Err(BackupError::Integrity));
    assert!(matches!(result, Err(BackupError::Integrity)));
    let result = directory.with_file_path("database", &control, |path| {
        std::fs::remove_file(path)?;
        std::fs::create_dir(path)?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn restore_provider_path_allows_provider_owned_regular_file_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let pinned = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    let directory = RestoreDirectory { directory: &pinned };
    drop(directory.create_file("database").unwrap());
    let control = StorageReadControl::with_limit(128 << 10);
    directory
        .with_file_path("database", &control, |path| {
            std::fs::remove_file(path)?;
            std::fs::write(path, b"compacted provider image")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        pinned.read("database").unwrap(),
        b"compacted provider image"
    );
    assert_eq!(control.memory().used(), 0);
}

#[cfg(unix)]
#[test]
fn restore_provider_path_follows_retained_directory_and_rejects_midcall_movement() {
    let temporary = tempfile::tempdir().unwrap();
    let initial = temporary.path().join("initial");
    let moved = temporary.path().join("moved");
    let final_path = temporary.path().join("final");
    std::fs::create_dir(&initial).unwrap();
    let pinned = Dir::open_ambient_dir(&initial, cap_std::ambient_authority()).unwrap();
    let directory = RestoreDirectory { directory: &pinned };
    drop(directory.create_file("database").unwrap());
    let control = StorageReadControl::with_limit(128 << 10);
    std::fs::rename(&initial, &moved).unwrap();
    std::fs::create_dir(&initial).unwrap();
    std::fs::write(initial.join("database"), b"unrelated file").unwrap();
    let result = directory.with_file_path("database", &control, |path| {
        assert_eq!(path, moved.canonicalize()?.join("database"));
        std::fs::write(path, b"completed before movement")?;
        std::fs::rename(&moved, &final_path)?;
        std::fs::create_dir(&moved)?;
        std::fs::write(moved.join("database"), b"replacement directory")?;
        Ok(())
    });
    assert!(result.is_err());
    directory
        .with_file_path("database", &control, |path| {
            assert_eq!(path, final_path.canonicalize()?.join("database"));
            assert_eq!(std::fs::read(path)?, b"completed before movement");
            Ok(())
        })
        .unwrap();
    assert_eq!(
        std::fs::read(initial.join("database")).unwrap(),
        b"unrelated file"
    );
    assert_eq!(
        std::fs::read(moved.join("database")).unwrap(),
        b"replacement directory"
    );
    assert_eq!(control.memory().used(), 0);
}
