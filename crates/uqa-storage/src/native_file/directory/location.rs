//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded pathname interoperation for providers that open private files by name.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use cap_std::fs::Dir;
use uqa_core::memory::{MemoryError, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(crate) struct FilePath {
    path: PathBuf,
    _charge: MemoryReservation,
}

impl FilePath {
    pub(crate) fn new(
        directory: &Dir,
        name: &OsStr,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        super::component(name).map_err(error)?;
        let path = platform(directory, name, control)?;
        path.verify(directory)?;
        Ok(path)
    }

    fn from_parent(
        parent: &OsStr,
        name: &OsStr,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if !Path::new(parent).is_absolute() {
            return Err(error(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory location is not absolute",
            )));
        }
        let bytes = parent
            .len()
            .checked_add(name.len())
            .and_then(|n| n.checked_add(1))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut charge = control.memory().reserve(bytes)?;
        let mut path = PathBuf::with_capacity(bytes);
        charge.grow(path.capacity().saturating_sub(charge.bytes()))?;
        path.push(parent);
        path.push(name);
        Ok(Self {
            path,
            _charge: charge,
        })
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }

    /// A provider path is admitted only while its parent still resolves to the retained directory. This check opens no ordinary database file and therefore cannot retire another connection's POSIX record locks.
    pub(crate) fn verify(&self, directory: &Dir) -> StorageBackendResult<()> {
        let resolved = Dir::open_ambient_dir(
            self.path.parent().expect("absolute child path"),
            cap_std::ambient_authority(),
        )
        .map_err(error)?;
        if super::identity(&resolved).map_err(error)?
            != super::identity(directory).map_err(error)?
        {
            return Err(error(io::Error::new(
                io::ErrorKind::InvalidData,
                "private directory pathname changed",
            )));
        }
        Ok(())
    }
}

fn error(error: io::Error) -> StorageBackendError {
    StorageBackendError::backend("native directory", error)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform(
    directory: &Dir,
    name: &OsStr,
    control: &StorageReadControl,
) -> StorageBackendResult<FilePath> {
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    const BYTES: usize = libc::PATH_MAX as usize;
    let _memory = control.memory().reserve(BYTES + 64)?;
    let mut link = [0u8; 64];
    write!(
        &mut &mut link[..63],
        "/proc/self/fd/{}",
        directory.as_raw_fd()
    )
    .map_err(error)?;
    let mut bytes = [0u8; BYTES];
    // SAFETY: link has a NUL-filled tail after its bounded descriptor name. The live directory owns that descriptor and bytes is a writable buffer of the stated size.
    let length =
        unsafe { libc::readlink(link.as_ptr().cast(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if length < 0 {
        return Err(error(io::Error::last_os_error()));
    }
    let length = length as usize;
    if length == bytes.len() {
        return Err(error(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory location exceeds the platform path bound",
        )));
    }
    FilePath::from_parent(OsStr::from_bytes(&bytes[..length]), name, control)
}

#[cfg(target_vendor = "apple")]
fn platform(
    directory: &Dir,
    name: &OsStr,
    control: &StorageReadControl,
) -> StorageBackendResult<FilePath> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    const BYTES: usize = libc::PATH_MAX as usize;
    let _memory = control.memory().reserve(BYTES)?;
    let mut bytes = [0u8; BYTES];
    // SAFETY: F_GETPATH receives the live descriptor and a MAXPATHLEN-sized writable buffer, as required by the public platform interface.
    if unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr()) } == -1 {
        return Err(error(io::Error::last_os_error()));
    }
    let length = bytes.iter().position(|byte| *byte == 0).ok_or_else(|| {
        error(io::Error::new(
            io::ErrorKind::InvalidData,
            "unterminated directory location",
        ))
    })?;
    FilePath::from_parent(OsStr::from_bytes(&bytes[..length]), name, control)
}

#[cfg(windows)]
fn platform(
    directory: &Dir,
    name: &OsStr,
    control: &StorageReadControl,
) -> StorageBackendResult<FilePath> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };
    const UNITS: usize = 32768;
    let mut buffer_memory = control.memory().reserve(2 * UNITS)?;
    let mut bytes = vec![0u16; UNITS];
    buffer_memory.grow((2 * bytes.capacity()).saturating_sub(buffer_memory.bytes()))?;
    // SAFETY: the directory owns the handle, and the writable UTF-16 buffer has the stated capacity including termination space.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            directory.as_raw_handle(),
            bytes.as_mut_ptr(),
            UNITS as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    } as usize;
    if length == 0 {
        return Err(error(io::Error::last_os_error()));
    }
    if length >= bytes.len() {
        return Err(error(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory location exceeds the platform path bound",
        )));
    }
    let mut memory = control.memory().reserve(3 * length)?;
    let parent = OsString::from_wide(&bytes[..length]);
    memory.grow(parent.capacity().saturating_sub(memory.bytes()))?;
    FilePath::from_parent(&parent, name, control)
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
)))]
fn platform(
    _directory: &Dir,
    _name: &OsStr,
    _control: &StorageReadControl,
) -> StorageBackendResult<FilePath> {
    Err(error(io::Error::new(
        io::ErrorKind::Unsupported,
        "native directory pathname lookup is unavailable",
    )))
}
