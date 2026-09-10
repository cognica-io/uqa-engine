//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native byte-range locking, positioned I/O, and process liveness.

use super::FileLockCoordinator;
#[cfg(unix)]
use super::{AsRawFd, FileExt};
#[cfg(windows)]
use super::{AsRawHandle, FileExt, MODE_TRANSITION_LOCK_BYTE};

impl FileLockCoordinator {
    #[cfg(unix)]
    pub(super) fn apply_byte_mode(
        &self,
        offset: u64,
        _before: Option<bool>,
        after: Option<bool>,
    ) -> std::io::Result<()> {
        let lock_type = match after {
            Some(true) => libc::F_WRLCK,
            Some(false) => libc::F_RDLCK,
            None => libc::F_UNLCK,
        };
        let mut flock: libc::flock = unsafe { std::mem::zeroed() };
        flock.l_type = lock_type as libc::c_short;
        flock.l_whence = libc::SEEK_SET as libc::c_short;
        flock.l_start = libc::off_t::try_from(offset).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "record-lock offset exceeds the platform off_t range",
            )
        })?;
        flock.l_len = 1;
        let result = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETLK, &flock) };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn apply_byte_mode(
        &self,
        offset: u64,
        before: Option<bool>,
        after: Option<bool>,
    ) -> std::io::Result<()> {
        if before == after {
            return Ok(());
        }
        while let Err(error) = windows_lock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE, true) {
            if !lock_would_block(&error) {
                return Err(error);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let transition = (|| {
            if before.is_some() {
                windows_unlock_byte(&self.file, offset)?;
            }
            let result = match after {
                Some(write) => windows_lock_byte(&self.file, offset, write),
                None => Ok(()),
            };
            if result.is_err() {
                if let Some(write) = before {
                    while windows_lock_byte(&self.file, offset, write).is_err() {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            result
        })();
        let unlock_transition = windows_unlock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE);
        transition.and(unlock_transition)
    }
}

#[cfg(unix)]
pub(super) fn read_exact_at(
    file: &std::fs::File,
    bytes: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
    file.read_exact_at(bytes, offset)
}

#[cfg(unix)]
pub(super) fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    file.write_all_at(bytes, offset)
}

#[cfg(windows)]
pub(super) fn read_exact_at(
    file: &std::fs::File,
    bytes: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
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
pub(super) fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
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
pub(super) fn lock_would_block(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES))
}

#[cfg(windows)]
pub(super) fn lock_would_block(error: &std::io::Error) -> bool {
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
fn windows_lock_byte(file: &std::fs::File, offset: u64, write: bool) -> std::io::Result<()> {
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
fn windows_unlock_byte(file: &std::fs::File, offset: u64) -> std::io::Result<()> {
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

#[cfg(unix)]
pub(super) fn process_alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(windows)]
pub(super) fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error()
            != Some(ERROR_INVALID_PARAMETER as i32);
    }
    let mut exit_code = 0u32;
    let queried = unsafe { GetExitCodeProcess(handle, &raw mut exit_code) } != 0;
    let _ = unsafe { CloseHandle(handle) };
    !queried || exit_code == STILL_ACTIVE as u32
}
