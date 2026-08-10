//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parent-directory synchronization for filesystem namespace changes.

#[cfg(unix)]
use super::File;
use super::Path;

// Test-only fault injection: the compaction regression test arms this flag
// to prove that a directory-sync failure cannot desynchronize container
// state from the file already renamed into place.
#[cfg(all(unix, test))]
thread_local! {
    pub(super) static FAIL_NEXT_PARENT_SYNC: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(unix)]
pub(super) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(std::cell::Cell::take) {
        return Err(std::io::Error::other(
            "injected parent directory sync failure",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        )
    })?;
    File::open(parent)?.sync_all()
}

/// Directory synchronization is not available through the portable file API
/// on non-Unix targets, so this operation is a no-op there.
#[cfg(not(unix))]
pub(super) fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
