//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;

use super::invalid;
use crate::read_control::StorageReadControl;
use crate::temporary_file::BlockTemporaryFile;
use crate::{StorageBackendError, StorageBackendResult};

pub(super) type File = BlockTemporaryFile<4096>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiskANNTemporaryError {
    #[error("temporary storage requires {required} bytes, exceeding limit {limit}")]
    Limit { required: u64, limit: u64 },
    #[error("temporary storage size overflow")]
    SizeOverflow,
}

impl From<DiskANNTemporaryError> for StorageBackendError {
    fn from(error: DiskANNTemporaryError) -> Self {
        StorageBackendError::backend("DiskANN build", error)
    }
}

#[derive(Debug)]
struct State {
    limit: u64,
    used: u64,
    peak: u64,
}

/// Shared allowance for encrypted file lengths, including framing and rounded blocks. Filesystem allocation metadata is separate; clones share the same live limit.
#[derive(Debug, Clone)]
pub struct DiskANNTemporaryBudget(Arc<Mutex<State>>);

impl DiskANNTemporaryBudget {
    pub fn new(limit: u64) -> Self {
        Self(Arc::new(Mutex::new(State {
            limit,
            used: 0,
            peak: 0,
        })))
    }

    pub fn limit(&self) -> u64 {
        self.0.lock().limit
    }
    pub fn used(&self) -> u64 {
        self.0.lock().used
    }
    pub fn peak(&self) -> u64 {
        self.0.lock().peak
    }
}

struct Reservation {
    budget: DiskANNTemporaryBudget,
    bytes: u64,
}

impl Reservation {
    fn grow_to(&mut self, bytes: u64) -> StorageBackendResult<()> {
        let additional = bytes.saturating_sub(self.bytes);
        let mut state = self.budget.0.lock();
        let required = state
            .used
            .checked_add(additional)
            .ok_or(DiskANNTemporaryError::SizeOverflow)?;
        if required > state.limit {
            return Err(DiskANNTemporaryError::Limit {
                required,
                limit: state.limit,
            }
            .into());
        }
        state.used = required;
        state.peak = state.peak.max(required);
        self.bytes += additional;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.lock().used -= self.bytes;
    }
}

pub(super) struct TemporaryRun {
    // The file closes before its physical-byte reservation is released.
    file: File,
    reservation: Reservation,
    length: u64,
    failed: bool,
}

impl TemporaryRun {
    pub(super) fn new(
        directory: &Path,
        budget: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            file: File::new_in(directory).map_err(io_error)?,
            reservation: Reservation {
                budget: budget.clone(),
                bytes: 0,
            },
            length: 0,
            failed: false,
        })
    }

    /// A failed write poisons this unpublished run. Its owner must discard it, retaining its entire reservation until the file closes.
    pub(super) fn append(
        &mut self,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if self.failed {
            return Err(invalid("temporary run previously failed"));
        }
        let end = self
            .length
            .checked_add(bytes.len() as u64)
            .ok_or(DiskANNTemporaryError::SizeOverflow)?;
        self.reservation
            .grow_to(File::physical_len_for(end).map_err(io_error)?)?;
        self.failed = true;
        self.file
            .seek(SeekFrom::Start(self.length))
            .map_err(io_error)?;
        for chunk in bytes.chunks(4096) {
            control.check()?;
            self.file.write_all(chunk).map_err(io_error)?;
        }
        control.check()?;
        self.length = end;
        self.failed = false;
        Ok(())
    }

    pub(super) fn read<T>(
        &self,
        control: &StorageReadControl,
        read: impl FnOnce(&mut File) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        control.check()?;
        if self.failed {
            return Err(invalid("temporary run previously failed"));
        }
        let mut file = self.file.reopen().map_err(io_error)?;
        let result = read(&mut file)?;
        control.check()?;
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        self.file.path()
    }

    #[cfg(test)]
    pub(super) fn block_io_counts(&self) -> (usize, u64) {
        self.file.block_io_counts()
    }
}

pub(super) fn io_error(error: std::io::Error) -> StorageBackendError {
    StorageBackendError::backend("DiskANN temporary storage", error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_partial_run_is_unreadable_and_retains_its_charge_until_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(4096);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let mut run = TemporaryRun::new(directory.path(), &temporary, &control).unwrap();
        let path = run.path().to_owned();
        run.file.fail_write_after(4096 + 24 + 2 + 16 + 1 + 30);
        assert!(matches!(
            run.append(&[1; 9000], &control),
            Err(StorageBackendError::Backend { .. })
        ));
        assert_eq!(temporary.used(), File::physical_len_for(9000).unwrap());
        assert!(std::fs::metadata(&path).unwrap().len() < temporary.used());
        assert!(run.read(&control, |_| Ok(())).is_err());
        assert!(run.append(&[2], &control).is_err());
        drop(run);
        assert_eq!(temporary.used(), 0);
        assert!(!path.exists());
    }
}
