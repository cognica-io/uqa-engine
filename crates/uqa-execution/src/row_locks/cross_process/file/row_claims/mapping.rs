//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional bounded transport for claim bytes, with unchanged arbitration and format.

use std::fs::File;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use uqa_storage::native_file::SharedFileMapping;

/// No table operation touches a mapping without the coordinator's local state
/// mutex and exclusive claim-table lock byte. Revalidate its extent on every
/// acquisition, and discard it before this process rebuilds or resets the file.
#[derive(Default)]
pub(in crate::row_locks::cross_process::file) struct Mapping {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    window: Option<SharedFileMapping>,
    unavailable: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAXIMUM_BYTES: u64 = 64 * 1024 * 1024;

// Unsupported targets keep the same instance API with no mapped state.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(clippy::unused_self)
)]
impl Mapping {
    pub(super) fn clear(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.window = None;
        }
    }

    pub(super) fn refresh(&mut self, file: &File) {
        if self.unavailable {
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let Ok(metadata) = file.metadata() else {
                self.clear();
                self.unavailable = true;
                return;
            };
            let length = metadata.len().min(MAXIMUM_BYTES) as usize;
            if self
                .window
                .as_ref()
                .is_some_and(|window| window.mapped_len() == length)
            {
                return;
            }
            self.clear();
            if length != 0 {
                match SharedFileMapping::new(file, length) {
                    Ok(window) => self.window = Some(window),
                    // Mapping is an optional transport. Allocation, filesystem
                    // or VM limits leave the original fallible I/O available.
                    Err(_) => self.unavailable = true,
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = file;
            self.unavailable = true;
        }
    }

    #[allow(unsafe_code)] // The coordinator owns external exclusion and validated extent.
    pub(super) fn read(&self, offset: u64, bytes: &mut [u8]) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: TableLock owns the local and cross-process locks, acquired
            // before refresh validated the extent; callers copy into private bytes.
            self.window
                .as_ref()
                .is_some_and(|window| unsafe { window.read(offset, bytes) })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (offset, bytes);
            false
        }
    }

    #[allow(unsafe_code)] // The coordinator owns external exclusion and validated extent.
    pub(super) fn write(&self, offset: u64, bytes: &[u8]) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use super::table::{ENTRY_SIZE, STATE_OFFSET, STATE_TOMBSTONE};
            let Some(window) = self
                .window
                .as_ref()
                .filter(|window| window.contains(offset, bytes.len()))
            else {
                return false;
            };
            assert!(bytes.len().is_multiple_of(ENTRY_SIZE as usize));
            for (index, entry) in bytes
                .as_chunks::<{ ENTRY_SIZE as usize }>()
                .0
                .iter()
                .enumerate()
            {
                // SAFETY: TableLock retains the validated extent and exclusion.
                // A tombstone decodes without inspecting any other byte, so a
                // killed writer leaves a valid probe chain, never a torn entry.
                unsafe {
                    window.write_published(
                        offset + index as u64 * ENTRY_SIZE,
                        entry,
                        STATE_OFFSET,
                        STATE_TOMBSTONE,
                    );
                }
            }
            true
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (offset, bytes);
            false
        }
    }

    #[cfg(test)]
    pub(super) fn disable(&mut self) {
        self.clear();
        self.unavailable = true;
    }

    #[cfg(test)]
    pub(super) fn active(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.window.is_some()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn a_large_claim_file_uses_bounded_mapping_and_rechecks_a_smaller_extent() {
        let file = tempfile::tempfile().unwrap();
        file.set_len(MAXIMUM_BYTES + 4096).unwrap();
        let mut mapping = Mapping::default();
        mapping.refresh(&file);
        if !mapping.active() {
            return;
        }
        assert_eq!(
            mapping.window.as_ref().unwrap().mapped_len() as u64,
            MAXIMUM_BYTES
        );
        let mut bytes = [0_u8; 32];
        assert!(mapping.read(MAXIMUM_BYTES - 32, &mut bytes));
        assert!(!mapping.read(MAXIMUM_BYTES, &mut bytes));
        assert!(!mapping.write(MAXIMUM_BYTES, &bytes));
        file.set_len(4096).unwrap();
        mapping.refresh(&file);
        assert!(mapping.read(0, &mut bytes));
        assert!(!mapping.read(4096, &mut bytes));
        assert_eq!(file.metadata().unwrap().len(), 4096);
    }
}
