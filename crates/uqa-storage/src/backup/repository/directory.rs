//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fs::{File, TryLockError};
use std::path::{Component, Path};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_std::fs::{Dir, OpenOptions};

use crate::read_control::StorageReadControl;

use super::super::{BackupError, BackupResult};

/// Only the caller-selected parent is opened with ambient authority. All repository entries use pinned relative directory capabilities and reject final-component symlinks.
pub(super) fn parent(path: &Path) -> BackupResult<(Dir, &std::ffi::OsStr)> {
    let name = path.file_name().ok_or(BackupError::Invalid(
        "repository path has no final component",
    ))?;
    if !matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) {
        return Err(BackupError::Invalid("repository path component"));
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok((
        Dir::open_ambient_dir(parent, cap_std::ambient_authority())?,
        name,
    ))
}

pub(super) fn child(parent: &Dir, name: &std::ffi::OsStr) -> BackupResult<Dir> {
    Ok(parent.open_dir_nofollow(name)?)
}

pub(super) fn create_directory(parent: &Dir, name: impl AsRef<Path>) -> BackupResult<()> {
    let builder = cap_std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use cap_std::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    parent.create_dir_with(name, &builder)?;
    Ok(())
}

pub(super) fn create_file(directory: &Dir, name: impl AsRef<Path>) -> BackupResult<File> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    Ok(directory.open_with(name, &options)?.into_std())
}

pub(super) fn open_file(
    directory: &Dir,
    name: impl AsRef<Path>,
    write: bool,
) -> BackupResult<File> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.read(true).write(write).follow(FollowSymlinks::No);
    let file = directory.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(BackupError::Invalid(
            "repository entry is not a regular file",
        ));
    }
    Ok(file.into_std())
}

/// A successful return certifies the platform's directory synchronization operation. Unsupported filesystems fail; no branch substitutes a no-op for durability.
pub(super) fn synchronize(directory: &Dir) -> BackupResult<()> {
    // A directory capability can be an O_PATH handle on Linux. Open an I/O handle relative to that pinned capability instead of cloning its flags.
    let mut options = OpenOptions::new();
    options
        .read(true)
        .maybe_dir(true)
        .follow(FollowSymlinks::No);
    #[cfg(windows)]
    options.write(true);
    directory.open_with(".", &options)?.sync_all()?;
    Ok(())
}

/// Each lease owns a separately opened file description. Closing the final File releases its OS lock, including after process death, without process-ID liveness guesses or repeated lock calls on cloned descriptors.
pub(super) fn lock(file: File, shared: bool, control: &StorageReadControl) -> BackupResult<File> {
    control.cancellation().check()?;
    let result = if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    };
    match result {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(BackupError::Busy),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(super) fn name(bytes: [u8; 16]) -> [u8; 32] {
    let mut name = [0; 32];
    for (output, byte) in name.chunks_exact_mut(2).zip(bytes) {
        output[0] = b"0123456789abcdef"[usize::from(byte >> 4)];
        output[1] = b"0123456789abcdef"[usize::from(byte & 15)];
    }
    name
}

pub(super) fn text(name: &[u8; 32]) -> &str {
    std::str::from_utf8(name).expect("hexadecimal repository identity")
}
