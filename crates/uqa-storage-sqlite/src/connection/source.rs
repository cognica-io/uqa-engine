//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native database pools never adopt a replacement pathname after opening.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use rusqlite::Connection;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use uqa_storage::native_file::PathChangeWatch;

use super::{ConnectionSpec, Result, SQLiteError};

#[cfg(unix)]
mod platform {
    use std::{os::unix::fs::MetadataExt, path::Path};

    pub(super) type Identity = (u64, u64);

    pub(super) fn path_identity(path: &Path) -> std::io::Result<Identity> {
        let metadata = path.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }
}

#[cfg(windows)]
mod platform {
    use std::{fs::File, mem::MaybeUninit, os::windows::io::AsRawHandle, path::Path};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    // Preserve all 128 bits on filesystems such as ReFS.
    pub(super) type Identity = (u64, [u8; 16]);

    #[allow(unsafe_code)] // This function is the checked Win32 file-identity boundary.
    fn identity(file: &File) -> std::io::Result<Identity> {
        let mut information = MaybeUninit::<FILE_ID_INFO>::uninit();
        // SAFETY: the borrowed file retains a live handle, and the output buffer
        // has exactly the layout and size required by FileIdInfo.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                information.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful FileIdInfo retrieval initialized the complete value.
        let information = unsafe { information.assume_init() };
        Ok((
            information.VolumeSerialNumber,
            information.FileId.Identifier,
        ))
    }

    pub(super) fn path_identity(path: &Path) -> std::io::Result<Identity> {
        identity(&File::open(path)?)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::path::Path;

    pub(super) type Identity = ();

    pub(super) fn path_identity(_path: &Path) -> std::io::Result<Identity> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "retained SQLite file identity is unavailable on this target",
        ))
    }
}

pub(super) struct DatabaseSource {
    path: PathBuf,
    identity: platform::Identity,
    changed: AtomicBool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    watch: parking_lot::Mutex<Option<PathChangeWatch>>,
    #[cfg(test)]
    identity_reads: std::sync::atomic::AtomicUsize,
}

impl DatabaseSource {
    pub(super) fn capture(spec: &mut ConnectionSpec, initial: &Connection) -> Result<Option<Self>> {
        let path = match spec {
            ConnectionSpec::File { path, .. } | ConnectionSpec::Auxiliary { path, .. }
                if !path.as_os_str().is_empty() && path != Path::new(":memory:") =>
            {
                path
            }
            // Compressed containers own a stable format identity across physical
            // compaction replacements; native inode checks do not apply to them.
            _ => return Ok(None),
        };
        // Freeze relative paths and symlink selection for every subsequent open.
        *path = path.canonicalize()?;
        // Install notifications before observing the identity they protect.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let watch = PathChangeWatch::new(path).ok();
        let source = Self {
            path: path.clone(),
            identity: platform::path_identity(path)?,
            changed: AtomicBool::new(false),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            watch: parking_lot::Mutex::new(watch),
            #[cfg(test)]
            identity_reads: std::sync::atomic::AtomicUsize::new(1),
        };
        source.check_connection(initial)?;
        Ok(Some(source))
    }

    fn invalidate<T>(&self) -> Result<T> {
        self.changed.store(true, Ordering::Release);
        Err(SQLiteError::DatabaseSourceChanged)
    }

    pub(super) fn check(&self) -> Result<()> {
        if self.changed.load(Ordering::Acquire) {
            return Err(SQLiteError::DatabaseSourceChanged);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut watch = self.watch.lock();
            let revalidate = watch
                .as_mut()
                .is_none_or(|current| current.changed().unwrap_or(true));
            if revalidate {
                // Install before checking, but adopt only after success. A transient
                // identity error must not leave a clean watch authorizing reuse.
                let replacement = watch
                    .as_ref()
                    .and_then(|_| PathChangeWatch::new(&self.path).ok());
                self.check_identity()?;
                *watch = replacement;
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        self.check_identity()?;
        if self.changed.load(Ordering::Acquire) {
            return Err(SQLiteError::DatabaseSourceChanged);
        }
        Ok(())
    }

    fn check_identity(&self) -> Result<()> {
        #[cfg(test)]
        self.identity_reads.fetch_add(1, Ordering::Relaxed);
        match platform::path_identity(&self.path) {
            Ok(identity) if identity == self.identity => {
                if self.changed.load(Ordering::Acquire) {
                    return Err(SQLiteError::DatabaseSourceChanged);
                }
                Ok(())
            }
            Ok(_) => self.invalidate(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.invalidate(),
            Err(error) => Err(error.into()),
        }
    }

    #[allow(unsafe_code)] // rusqlite exposes file-control only through its raw SQLite handle.
    pub(super) fn check_connection(&self, connection: &Connection) -> Result<()> {
        self.check()?;
        let mut moved: std::ffi::c_int = 0;
        // SAFETY: rusqlite retains the live connection exclusively on this thread;
        // main is NUL-terminated and HAS_MOVED writes one live c_int.
        let code = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                std::ptr::addr_of_mut!(moved).cast(),
            )
        };
        match code {
            rusqlite::ffi::SQLITE_OK if moved != 0 => self.invalidate(),
            rusqlite::ffi::SQLITE_OK | rusqlite::ffi::SQLITE_NOTFOUND => Ok(()),
            _ => Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into()),
        }
    }
}

#[cfg(test)]
mod tests;
