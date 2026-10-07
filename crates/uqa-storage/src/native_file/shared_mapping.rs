//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared bytes of a local coordination file, accessed only under its external lock.

use std::{fs::File, io, os::fd::AsRawFd, ptr::NonNull};

mod allocation;

/// A retained mapping, without a second file descriptor or borrowed Rust slices.
/// Mapping ownership may move between threads; byte access requires external
/// exclusion, extent validation and memory barriers at the lock boundary.
pub struct SharedFileMapping {
    address: NonNull<u8>,
    length: usize,
}

// SAFETY: moving the mapping changes no address or ownership. Access is unsafe
// and requires external exclusion; this type deliberately does not implement Sync.
unsafe impl Send for SharedFileMapping {}

impl SharedFileMapping {
    /// Map an existing prefix, reserving sparse holes before writable access.
    /// The caller must serialize this operation with file writes and resizing.
    /// The existing file bytes and length are preserved, as is its seek position.
    /// Unsupported filesystems or allocation failures should use positioned I/O.
    pub fn new(file: &File, length: usize) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || length == 0
            || length > isize::MAX as usize
            || length as u64 > metadata.len()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid mapped file extent",
            ));
        }
        require_local_filesystem(file)?;
        allocation::reserve(file, length)?;
        // SAFETY: mmap borrows an open read/write file, with a checked nonzero
        // existing extent; it creates a new mapping without replacing any address.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(address) = NonNull::new(address.cast::<u8>()) else {
            // SAFETY: even a zero-address mapping must be released on rejection.
            unsafe { libc::munmap(address, length) };
            return Err(io::Error::other("mapped file has a null address"));
        };
        Ok(Self { address, length })
    }

    pub fn mapped_len(&self) -> usize {
        self.length
    }

    pub fn contains(&self, offset: u64, length: usize) -> bool {
        self.start(offset, length).is_some()
    }

    fn start(&self, offset: u64, length: usize) -> Option<usize> {
        let start = usize::try_from(offset).ok()?;
        (start.checked_add(length)? <= self.length).then_some(start)
    }

    /// Copy a mapped range, returning false if it is outside this mapping.
    ///
    /// # Safety
    /// The underlying file must still cover the mapped extent. Exclude writes
    /// and resizing in every process for the complete copy, with an acquire
    /// barrier after taking the external lock. The destination must not alias
    /// the mapping. No reference into the mapped memory escapes this call.
    pub unsafe fn read(&self, offset: u64, bytes: &mut [u8]) -> bool {
        let Some(start) = self.start(offset, bytes.len()) else {
            return false;
        };
        // SAFETY: the caller maintains the live extent and exclusion; start
        // bounds the complete copy, whose destination is separately owned.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.address.as_ptr().add(start),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
        }
        true
    }

    /// Replace a mapped range, returning false if it is outside this mapping.
    ///
    /// # Safety
    /// The underlying file must still cover the reserved mapped extent. Exclude
    /// all other access and resizing until the copy completes, then issue a
    /// release barrier before unlocking. The source must not alias the mapping.
    /// This publishes coordination bytes; it does not provide crash durability.
    pub unsafe fn write(&self, offset: u64, bytes: &[u8]) -> bool {
        let Some(start) = self.start(offset, bytes.len()) else {
            return false;
        };
        // SAFETY: the caller maintains the reserved extent and exclusive access;
        // start bounds the complete copy from a separately owned source.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.address.as_ptr().add(start),
                bytes.len(),
            );
        }
        true
    }

    /// Replace a record while publishing its state byte last. An interrupted
    /// copy leaves `pending`, so a peer never decodes a partially written record.
    /// Returns false without mutation when either range is invalid.
    ///
    /// # Safety
    /// The exclusion, extent and source requirements of `write` apply. `pending`
    /// must describe a valid unpublished record in the caller's format, regardless
    /// of its remaining bytes. Its state byte must be independently writable.
    pub unsafe fn write_published(
        &self,
        offset: u64,
        bytes: &[u8],
        state: usize,
        pending: u8,
    ) -> bool {
        use std::sync::atomic::{AtomicU8, Ordering};
        let Some(start) = self
            .start(offset, bytes.len())
            .filter(|_| state < bytes.len())
        else {
            return false;
        };
        // SAFETY: the validated state byte is aligned for AtomicU8. Exclusion
        // prevents concurrent atomic/non-atomic access to the mapped bytes.
        let publication = unsafe { AtomicU8::from_ptr(self.address.as_ptr().add(start + state)) };
        // SeqCst also orders the following payload copies after invalidation.
        publication.store(pending, Ordering::SeqCst);
        unsafe {
            self.write(offset, &bytes[..state]);
            self.write(offset + state as u64 + 1, &bytes[state + 1..]);
        }
        #[cfg(test)]
        assert!(
            !INTERRUPT_PUBLICATION.replace(false),
            "interrupted record publication"
        );
        publication.store(bytes[state], Ordering::Release);
        true
    }
}

#[cfg(test)]
thread_local! {
    static INTERRUPT_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Drop for SharedFileMapping {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns this mapping and no borrowed slice
        // exists. Unmapping neither opens nor closes a coordination descriptor.
        unsafe { libc::munmap(self.address.as_ptr().cast(), self.length) };
    }
}

fn require_local_filesystem(file: &File) -> io::Result<()> {
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor and correctly sized output are live for fstatfs.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    #[cfg(target_os = "linux")]
    let supported = matches!(
        filesystem.f_type as u64,
        0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994 | 0x8584_58f6
    );
    #[cfg(target_os = "macos")]
    let supported = {
        let name = unsafe { std::ffi::CStr::from_ptr(filesystem.f_fstypename.as_ptr()) };
        filesystem.f_flags & libc::MNT_LOCAL as u32 != 0
            && matches!(name.to_bytes(), b"apfs" | b"hfs")
    };
    if supported {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file mapping requires a supported local filesystem",
        ))
    }
}

#[cfg(test)]
mod tests;
