//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Handle-relative directory identity and atomic publication without replacement.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path};

use cap_std::fs::Dir;

pub(crate) const MAX_COMPONENT_BYTES: usize = 1024;

pub(crate) fn component(name: &OsStr) -> io::Result<()> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(value)) if value == name)
        || parts.next().is_some()
        || name.as_encoded_bytes().contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory component",
        ));
    }
    #[cfg(windows)]
    if name.as_encoded_bytes().contains(&b':')
        || name.as_encoded_bytes().ends_with(b".")
        || name.as_encoded_bytes().ends_with(b" ")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows directory component",
        ));
    }
    if name.as_encoded_bytes().len() > MAX_COMPONENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory component length",
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn identity(directory: &Dir) -> io::Result<[u8; 24]> {
    use cap_std::fs::MetadataExt;
    let metadata = directory.dir_metadata()?;
    let mut bytes = [0; 24];
    bytes[..8].copy_from_slice(&metadata.dev().to_le_bytes());
    bytes[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
    Ok(bytes)
}

#[cfg(windows)]
pub(crate) fn identity(directory: &Dir) -> io::Result<[u8; 24]> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };
    let mut information = FILE_ID_INFO::default();
    // SAFETY: the live directory owns the handle and the output has the exact SDK layout/size.
    let result = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileIdInfo,
            (&raw mut information).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut bytes = [0; 24];
    bytes[..8].copy_from_slice(&information.VolumeSerialNumber.to_le_bytes());
    bytes[8..].copy_from_slice(&information.FileId.Identifier);
    Ok(bytes)
}

/// Both names are single components beneath pinned directories. Existing destinations of every kind cause an error; unsupported filesystem operations are never replaced with a check-then-rename fallback.
pub(crate) fn rename_new(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    component(source)?;
    component(target)?;
    rename_platform(from, source, to, target)
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_platform(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let mut source_bytes = [0u8; MAX_COMPONENT_BYTES + 1];
    let mut target_bytes = [0u8; MAX_COMPONENT_BYTES + 1];
    source_bytes[..source.as_bytes().len()].copy_from_slice(source.as_bytes());
    target_bytes[..target.as_bytes().len()].copy_from_slice(target.as_bytes());
    // SAFETY: names are validated single, NUL-free components with bounded NUL-terminated buffers. Both directory descriptors remain owned throughout the syscall.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let result = unsafe {
        libc::renameat2(
            from.as_raw_fd(),
            source_bytes.as_ptr().cast(),
            to.as_raw_fd(),
            target_bytes.as_ptr().cast(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_vendor = "apple")]
    let result = unsafe {
        libc::renameatx_np(
            from.as_raw_fd(),
            source_bytes.as_ptr().cast(),
            to.as_raw_fd(),
            target_bytes.as_ptr().cast(),
            libc::RENAME_EXCL,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn rename_platform(from: &Dir, source: &OsStr, to: &Dir, target: &OsStr) -> io::Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, DELETE, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    #[repr(C)]
    struct RenameInfo {
        flags: u32,
        root: HANDLE,
        length: u32,
        name: [u16; MAX_COMPONENT_BYTES / 2],
    }
    const {
        assert!(
            std::mem::offset_of!(RenameInfo, name)
                == std::mem::offset_of!(FILE_RENAME_INFO, FileName)
        );
    }
    let mut information = RenameInfo {
        flags: 0,
        root: to.as_raw_handle(),
        length: 0,
        name: [0; MAX_COMPONENT_BYTES / 2],
    };
    for (index, character) in target.encode_wide().enumerate() {
        let slot = information.name.get_mut(index).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory component length")
        })?;
        *slot = character;
        information.length += 2;
    }
    let mut options = OpenOptions::new();
    options
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .maybe_dir(true)
        .follow(FollowSymlinks::No);
    let source = from.open_with(source, &options)?;
    if !source.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication source directory",
        ));
    }
    // SAFETY: RenameInfo has the SDK prefix layout and a bounded inline UTF-16 tail. The source and destination handles are live; zero flags selects ReplaceIfExists=false.
    let result = unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle(),
            FileRenameInfo,
            (&raw const information).cast(),
            std::mem::size_of::<RenameInfo>() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
)))]
fn rename_platform(_: &Dir, _: &OsStr, _: &Dir, _: &OsStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exclusive directory publication",
    ))
}

#[cfg(test)]
mod tests;
