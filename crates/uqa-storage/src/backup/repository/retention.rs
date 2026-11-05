//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authenticated root discovery and generation-validated repository reclamation.

mod collection;
mod reachability;

pub use collection::BackupCollectionReport;
#[cfg(test)]
pub(super) use collection::ON_DISCOVERY;

use crate::{
    backup::{BackupError, BackupId, BackupResult, BackupRoot},
    read_control::StorageReadControl,
    spill_map::{Map, Record},
    temporary_file::FileBudget,
    StorageBackendResult,
};

use super::{directory, LocalRepository};

struct Root(BackupRoot);

impl Record for Root {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(BackupRoot::ENCODED_BYTES)
    }
    fn encode(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.0.encode());
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        BackupRoot::decode(bytes)
            .map(Self)
            .map_err(|_| crate::spill_map::invalid("backup root"))
    }
}

/// A stable metadata listing ordered by backup ID. It does not retain roots
/// against later removal; open a `BackupLease` before reading an image. Listing
/// storage shares the caller's memory allowance and encrypted workspace limit.
pub struct BackupListing {
    roots: Map<Root>,
    after: Option<u128>,
    control: StorageReadControl,
    finished: bool,
    failed: bool,
}

impl BackupListing {
    pub fn next_backup(&mut self) -> BackupResult<Option<BackupRoot>> {
        if self.failed {
            return Err(BackupError::Finished);
        }
        if self.finished {
            return Ok(None);
        }
        let result = (|| {
            self.control.cancellation().check()?;
            let Some((key, root)) = self.roots.next(self.after)? else {
                return Ok(None);
            };
            self.after = Some(key);
            Ok(Some(root.0))
        })();
        if !matches!(result, Ok(Some(_))) {
            self.roots.clear();
            self.finished = true;
            self.failed = result.is_err();
        }
        result
    }
}

impl LocalRepository {
    /// List authenticated completed descriptors from one repository generation.
    /// Concurrent root/object publication or retirement returns `Busy` before
    /// exposing a partial listing. This does not perform full image verification.
    pub fn list_backups(
        &self,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupListing> {
        let generation = self.retention_generation(control)?;
        let files = FileBudget::new(maximum_workspace_bytes);
        let roots = self.scan_roots(&files, control);
        #[cfg(test)]
        if let Some(observe) = ON_LIST_DISCOVERY.with(|slot| slot.borrow_mut().take()) {
            observe();
        }
        let _admission = self.admission(control)?;
        if self.generation(control)? != generation {
            return Err(BackupError::Busy);
        }
        Ok(BackupListing {
            roots: roots?,
            after: None,
            control: control.clone(),
            finished: false,
            failed: false,
        })
    }

    fn retention_generation(&self, control: &StorageReadControl) -> BackupResult<[u8; 16]> {
        let _admission = self.admission(control)?;
        self.generation(control)
    }

    fn scan_roots(
        &self,
        files: &FileBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Map<Root>> {
        let mut roots = Map::with_file_budget(
            control.memory(),
            (control.memory().available() / 8).min(32 * 1024),
            files,
        );
        for entry in self.roots.entries()? {
            control.cancellation().check()?;
            let id = directory::identity(&entry?.file_name())?;
            roots.insert(
                u128::from_be_bytes(id),
                Root(self.read_root(BackupId::from_bytes(id)?, control)?),
                Some(control),
            )?;
        }
        Ok(roots)
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static ON_LIST_DISCOVERY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
