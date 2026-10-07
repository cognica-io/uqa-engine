//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve backing blocks so sparse coordination files do not first allocate on a mapped write.

use std::{fs::File, io, os::fd::AsRawFd};

#[cfg(target_os = "linux")]
pub(super) fn reserve(file: &File, length: usize) -> io::Result<()> {
    // SAFETY: the descriptor is live and the checked extent already exists.
    // KEEP_SIZE preserves the logical file size. Unsupported allocation falls
    // back to positioned writes instead of libc's write-based emulation.
    if unsafe {
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_KEEP_SIZE,
            0,
            length as libc::off_t,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
pub(super) fn reserve(file: &File, length: usize) -> io::Result<()> {
    use std::os::unix::fs::FileExt;

    // F_PREALLOCATE appends at the physical EOF rather than filling arbitrary
    // sparse holes. Materialize just those holes, keeping existing data intact.
    let original = seek(file, 0, libc::SEEK_CUR)?;
    let result = (|| {
        let mut start = 0;
        let zeros = [0_u8; 16 * 1024];
        while start < length as u64 {
            let hole = seek(file, start, libc::SEEK_HOLE)?;
            if hole >= length as u64 {
                break;
            }
            let end = match seek(file, hole, libc::SEEK_DATA) {
                Ok(data) => data.min(length as u64),
                Err(error) if error.raw_os_error() == Some(libc::ENXIO) => length as u64,
                Err(error) => return Err(error),
            };
            if end <= hole {
                return Err(io::Error::other("sparse file extent did not advance"));
            }
            let mut at = hole;
            while at < end {
                let count = zeros.len().min((end - at) as usize);
                file.write_all_at(&zeros[..count], at)?;
                at += count as u64;
            }
            start = end;
        }
        Ok(())
    })();
    let restored = seek(file, original, libc::SEEK_SET);
    result.and(restored.map(|_| ()))
}

#[cfg(target_os = "macos")]
fn seek(file: &File, offset: u64, mode: libc::c_int) -> io::Result<u64> {
    // SAFETY: all file users serialize under the same coordination lock, and
    // the original seek position is restored before returning to the caller.
    let result = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, mode) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as u64)
    }
}
