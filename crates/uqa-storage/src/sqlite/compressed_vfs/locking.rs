//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-process rollback-journal locks for the compressed `SQLite` VFS.

use std::io;

use super::{
    c_int, fs, File, FileExt, OpenOptions, Path, PathBuf, SQLITE_LOCK_EXCLUSIVE, SQLITE_LOCK_NONE,
    SQLITE_LOCK_PENDING, SQLITE_LOCK_RESERVED, SQLITE_LOCK_SHARED,
};

pub(super) struct FileLocks {
    shared: File,
    reserved: File,
    pending: File,
    level: c_int,
    owns_reservation: bool,
}

impl FileLocks {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let paths = lock_paths(path);
        let open = |path: &Path| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
        };
        Ok(Self {
            shared: open(&paths[0])?,
            reserved: open(&paths[1])?,
            pending: open(&paths[2])?,
            level: SQLITE_LOCK_NONE,
            owns_reservation: false,
        })
    }

    pub(super) const fn level(&self) -> c_int {
        self.level
    }

    pub(super) fn lock(&mut self, requested: c_int) -> io::Result<bool> {
        if requested <= self.level {
            return Ok(true);
        }
        match requested {
            SQLITE_LOCK_SHARED => self.acquire_shared(),
            SQLITE_LOCK_RESERVED => {
                if self.level != SQLITE_LOCK_SHARED {
                    return Err(invalid_transition());
                }
                if !try_lock(FileExt::try_lock_exclusive(&self.reserved))? {
                    return Ok(false);
                }
                self.owns_reservation = true;
                self.level = SQLITE_LOCK_RESERVED;
                Ok(true)
            }
            SQLITE_LOCK_PENDING | SQLITE_LOCK_EXCLUSIVE => self.acquire_exclusive(requested),
            _ => Err(invalid_transition()),
        }
    }

    fn acquire_shared(&mut self) -> io::Result<bool> {
        // A pending writer closes the reader gate before waiting for existing readers to leave.
        if !try_lock(FileExt::try_lock_shared(&self.pending))? {
            return Ok(false);
        }
        let result = try_lock(FileExt::try_lock_shared(&self.shared));
        let release = FileExt::unlock(&self.pending);
        match result {
            Ok(true) => {
                self.level = SQLITE_LOCK_SHARED;
                release?;
                Ok(true)
            }
            other => {
                release?;
                other
            }
        }
    }

    fn acquire_exclusive(&mut self, requested: c_int) -> io::Result<bool> {
        if self.level < SQLITE_LOCK_SHARED {
            return Err(invalid_transition());
        }
        if self.level < SQLITE_LOCK_PENDING {
            if !try_lock(FileExt::try_lock_exclusive(&self.pending))? {
                return Ok(false);
            }
            self.level = SQLITE_LOCK_PENDING;
        }
        if requested == SQLITE_LOCK_PENDING {
            return Ok(true);
        }
        // Windows cannot convert a shared lock in place. The pending gate protects the conversion gap from readers and competing writers.
        FileExt::unlock(&self.shared)?;
        match try_lock(FileExt::try_lock_exclusive(&self.shared)) {
            Ok(true) => {
                self.level = SQLITE_LOCK_EXCLUSIVE;
                Ok(true)
            }
            other => {
                FileExt::try_lock_shared(&self.shared)?;
                other
            }
        }
    }

    pub(super) fn unlock(&mut self, requested: c_int) -> io::Result<()> {
        if !matches!(requested, SQLITE_LOCK_NONE | SQLITE_LOCK_SHARED) {
            return Err(invalid_transition());
        }
        if requested >= self.level {
            return Ok(());
        }
        if requested == SQLITE_LOCK_NONE {
            let shared = FileExt::unlock(&self.shared);
            let reserved = self.release_reservation();
            let pending = self.release_pending();
            self.level = SQLITE_LOCK_NONE;
            return shared.and(reserved).and(pending);
        }
        if self.level == SQLITE_LOCK_EXCLUSIVE {
            FileExt::unlock(&self.shared)?;
            FileExt::try_lock_shared(&self.shared)?;
        }
        self.release_reservation()?;
        self.release_pending()?;
        self.level = SQLITE_LOCK_SHARED;
        Ok(())
    }

    fn release_reservation(&mut self) -> io::Result<()> {
        if self.owns_reservation {
            FileExt::unlock(&self.reserved)?;
            self.owns_reservation = false;
        }
        Ok(())
    }

    fn release_pending(&self) -> io::Result<()> {
        if self.level >= SQLITE_LOCK_PENDING {
            FileExt::unlock(&self.pending)?;
        }
        Ok(())
    }

    pub(super) fn check_reserved(&self) -> io::Result<bool> {
        if self.owns_reservation {
            return Ok(true);
        }
        if try_lock(FileExt::try_lock_exclusive(&self.reserved))? {
            FileExt::unlock(&self.reserved)?;
            Ok(false)
        } else {
            Ok(true)
        }
    }
}

pub(super) fn lock_paths(path: &Path) -> [PathBuf; 3] {
    [".lock", ".lock.reserved", ".lock.pending"].map(|suffix| {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    })
}

fn try_lock(result: io::Result<()>) -> io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if contended(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

fn contended(error: &io::Error) -> bool {
    #[cfg(not(target_os = "emscripten"))]
    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
        return true;
    }
    error.kind() == io::ErrorKind::WouldBlock
}

fn invalid_transition() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid SQLite file lock transition",
    )
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests;
