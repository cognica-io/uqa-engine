//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A one-shot invalidation gate for a canonical local filesystem pathname.

use std::{io, path::Path};

#[cfg(target_os = "linux")]
#[path = "path_watch/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "path_watch/macos.rs"]
mod platform;

/// Watches the containing directory, every ancestor's own identity, and mount
/// changes. Construct before validating the pathname. After any event or error,
/// validate again with a new watch; this watch stays invalidated. Unsupported
/// filesystems and platforms require ordinary pathname validation on every call.
pub struct PathChangeWatch {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    watch: platform::Watch,
    process: u32,
    changed: bool,
}

impl PathChangeWatch {
    pub fn new(path: &Path) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            }) {
                return Err(unsupported());
            }
            let parent = path
                .parent()
                .filter(|_| path.is_absolute())
                .ok_or_else(unsupported)?;
            let watch = platform::Watch::new(parent)?;
            // A replaced leaf may be a symlink to the original inode outside
            // this directory tree. Its target can change without a watched
            // directory event. Check after subscribing so a concurrent leaf
            // replacement still invalidates the watch.
            if path.symlink_metadata()?.file_type().is_symlink() {
                return Err(unsupported());
            }
            Ok(Self {
                watch,
                process: std::process::id(),
                changed: false,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    /// Whether the pathname must be revalidated. This never waits for an event.
    pub fn changed(&mut self) -> io::Result<bool> {
        if self.changed || self.process != std::process::id() {
            return Ok(true);
        }
        // A failed poll cannot authorize any subsequent reuse of this watch.
        self.changed = true;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.changed = self.watch.changed()?;
        }
        Ok(self.changed)
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "pathname requires direct identity checks",
    )
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
