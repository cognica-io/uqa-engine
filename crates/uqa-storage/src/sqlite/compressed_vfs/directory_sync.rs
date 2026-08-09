//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parent-directory synchronization for filesystem namespace changes.

use super::{File, Path};

#[cfg(unix)]
pub(super) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
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
