//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native byte locks and positioned I/O shared by physical coordination adapters. On POSIX, each adapter must keep one process-wide descriptor per coordination file: closing any other descriptor would release that process's record locks. Callers own local arbitration, lock ordering and cancellation.

#[cfg(windows)]
use std::os::windows::{fs::FileExt, io::AsRawHandle};
#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::fs::FileExt};

#[cfg(unix)]
pub fn try_lock_byte(file: &std::fs::File, offset: u64, write: bool) -> std::io::Result<()> {
    unix_byte_mode(
        file,
        offset,
        if write {
            libc::F_WRLCK as libc::c_short
        } else {
            libc::F_RDLCK as libc::c_short
        },
    )
}

#[cfg(unix)]
pub fn unlock_byte(file: &std::fs::File, offset: u64) -> std::io::Result<()> {
    unix_byte_mode(file, offset, libc::F_UNLCK as libc::c_short)
}

#[cfg(unix)]
fn unix_byte_mode(file: &std::fs::File, offset: u64, mode: libc::c_short) -> std::io::Result<()> {
    // SAFETY: flock is initialized before fcntl borrows it, and File owns the descriptor for the call.
    let mut flock: libc::flock = unsafe { std::mem::zeroed() };
    flock.l_type = mode;
    flock.l_whence = libc::SEEK_SET as libc::c_short;
    flock.l_start = libc::off_t::try_from(offset).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "record-lock offset exceeds the platform off_t range",
        )
    })?;
    flock.l_len = 1;
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &flock) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.read_exact_at(bytes, offset)
}

#[cfg(unix)]
pub fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    file.write_all_at(bytes, offset)
}

#[cfg(windows)]
pub fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut consumed = 0usize;
    while consumed < bytes.len() {
        let read = file.seek_read(
            &mut bytes[consumed..],
            offset.saturating_add(consumed as u64),
        )?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "positioned file read reached end of file",
            ));
        }
        consumed += read;
    }
    Ok(())
}

#[cfg(windows)]
pub fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    let mut consumed = 0usize;
    while consumed < bytes.len() {
        let written =
            file.seek_write(&bytes[consumed..], offset.saturating_add(consumed as u64))?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "positioned file write returned zero bytes",
            ));
        }
        consumed += written;
    }
    Ok(())
}

#[cfg(unix)]
pub fn lock_would_block(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES))
}

#[cfg(windows)]
pub fn lock_would_block(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32)
}

#[cfg(windows)]
fn windows_overlapped(offset: u64) -> windows_sys::Win32::System::IO::OVERLAPPED {
    let mut overlapped = windows_sys::Win32::System::IO::OVERLAPPED::default();
    overlapped.Anonymous = windows_sys::Win32::System::IO::OVERLAPPED_0 {
        Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0_0 {
            Offset: offset as u32,
            OffsetHigh: (offset >> 32) as u32,
        },
    };
    overlapped
}

#[cfg(windows)]
pub fn try_lock_byte(file: &std::fs::File, offset: u64, write: bool) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    let mut overlapped = windows_overlapped(offset);
    let flags = LOCKFILE_FAIL_IMMEDIATELY | if write { LOCKFILE_EXCLUSIVE_LOCK } else { 0 };
    let result = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &raw mut overlapped) };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub fn unlock_byte(file: &std::fs::File, offset: u64) -> std::io::Result<()> {
    let mut overlapped = windows_overlapped(offset);
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::UnlockFileEx(
            file.as_raw_handle(),
            0,
            1,
            0,
            &raw mut overlapped,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
