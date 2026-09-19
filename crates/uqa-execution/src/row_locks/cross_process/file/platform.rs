//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native byte-range locking, positioned I/O, and process liveness.

use super::FileLockCoordinator;
#[cfg(windows)]
use super::MODE_TRANSITION_LOCK_BYTE;
pub(super) use uqa_storage::native_file::{lock_would_block, read_exact_at, write_all_at};
use uqa_storage::native_file::{try_lock_byte, unlock_byte};

impl FileLockCoordinator {
    #[cfg(unix)]
    pub(super) fn apply_byte_mode(
        &self,
        offset: u64,
        _before: Option<bool>,
        after: Option<bool>,
    ) -> std::io::Result<()> {
        match after {
            Some(write) => try_lock_byte(&self.file, offset, write),
            None => unlock_byte(&self.file, offset),
        }
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
        while let Err(error) = try_lock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE, true) {
            if !lock_would_block(&error) {
                return Err(error);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let transition = (|| {
            if before.is_some() {
                unlock_byte(&self.file, offset)?;
            }
            let result = match after {
                Some(write) => try_lock_byte(&self.file, offset, write),
                None => Ok(()),
            };
            if result.is_err() {
                if let Some(write) = before {
                    while try_lock_byte(&self.file, offset, write).is_err() {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            result
        })();
        let unlock_transition = unlock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE);
        transition.and(unlock_transition)
    }
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
