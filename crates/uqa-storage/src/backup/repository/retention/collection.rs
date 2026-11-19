//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use cap_std::fs::Dir;
use uqa_core::memory::BudgetedVec;

use crate::{
    backup::{BackupError, BackupId, BackupResult},
    read_control::StorageReadControl,
    spill_map::Map,
    temporary_file::FileBudget,
};

use super::{
    super::{directory, LocalRepository},
    reachability::Reachability,
    Root,
};

/// Encoded file lengths and authenticated reference extents from one collection
/// pass. Removed bytes count repository directory entries, not physical blocks;
/// hard links and filesystem allocation remain outside this accounting boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupCollectionReport {
    pub completed_backups: u64,
    pub prepared_backups: u64,
    pub active_operations: u64,
    pub referenced_frame_bytes: u64,
    pub retained_object_bytes: u64,
    pub unreferenced_object_bytes: u64,
    pub removed_objects: u64,
    pub removed_object_bytes: u64,
    pub removed_staging_files: u64,
    pub removed_staging_bytes: u64,
    pub remaining_files: u64,
    pub workspace_peak_bytes: u64,
}

struct Candidate {
    operation: Option<BackupId>,
    object: [u8; 16],
    bytes: u64,
}

struct Collection {
    reachable: Reachability,
    candidates: BudgetedVec<Candidate>,
    maximum: usize,
    report: BackupCollectionReport,
}

impl Collection {
    fn candidate(&mut self, candidate: Candidate) -> BackupResult<()> {
        add(&mut self.report.remaining_files, 1)?;
        if self.candidates.len() < self.maximum {
            self.candidates.push(candidate)?;
        }
        Ok(())
    }

    fn operations(
        &mut self,
        repository: &LocalRepository,
        roots: &Map<Root>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        for entry in repository.operations.entries()? {
            control.cancellation().check()?;
            let name = entry?.file_name();
            let id = BackupId::from_bytes(directory::identity(&name)?)?;
            let operation = directory::child(&repository.operations, &name)?;
            let lease_file = match directory::open_file(&operation, "lease", true) {
                Ok(file) => file,
                Err(BackupError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound
                        && operation.entries()?.next().is_none()
                        && !roots.contains_key(u128::from_be_bytes(id.as_bytes()))? =>
                {
                    // Creation can stop before the lease file exists. Such an
                    // empty operation has neither a resumable cut nor objects.
                    continue;
                }
                Err(error) => return Err(error),
            };
            let lease = match directory::lock(lease_file, true, control) {
                Ok(lease) => Some(lease),
                Err(BackupError::Busy) => None,
                Err(error) => return Err(error),
            };
            if lease.is_none() {
                add(&mut self.report.active_operations, 1)?;
            }
            if !roots.contains_key(u128::from_be_bytes(id.as_bytes()))? {
                if let Some(root) = repository.prepared_root(&operation, id, control)? {
                    self.reachable.retain_root(repository, root, control)?;
                    add(&mut self.report.prepared_backups, 1)?;
                }
            }
            self.staging(&operation, id, lease.is_some(), control)?;
        }
        Ok(())
    }

    fn staging(
        &mut self,
        operation: &Dir,
        id: BackupId,
        inactive: bool,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        for entry in operation.entries()? {
            control.cancellation().check()?;
            let name = entry?.file_name();
            if matches!(
                name.to_str(),
                Some("lease" | "intent" | "prepared" | "cancelled")
            ) {
                continue;
            }
            let object = directory::identity(&name)?;
            if inactive {
                let bytes = directory::open_file(operation, &name, false)?
                    .metadata()?
                    .len();
                self.candidate(Candidate {
                    operation: Some(id),
                    object,
                    bytes,
                })?;
            } else {
                // An active capture still owns its private object names. Any
                // installation missed by this scan must change the generation.
                self.reachable.retain_pending(object, control)?;
            }
        }
        Ok(())
    }

    fn objects(
        &mut self,
        repository: &LocalRepository,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.reachable.validate_objects(repository, control)?;
        self.report.referenced_frame_bytes = self.reachable.frame_bytes(control)?;
        for entry in repository.objects.entries()? {
            control.cancellation().check()?;
            let name = entry?.file_name();
            let object = directory::identity(&name)?;
            let bytes = directory::open_file(&repository.objects, &name, false)?
                .metadata()?
                .len();
            if self.reachable.contains(object)? {
                add(&mut self.report.retained_object_bytes, bytes)?;
            } else {
                add(&mut self.report.unreferenced_object_bytes, bytes)?;
                self.candidate(Candidate {
                    operation: None,
                    object,
                    bytes,
                })?;
            }
        }
        Ok(())
    }

    fn remove(
        &mut self,
        repository: &LocalRepository,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        for candidate in self.candidates.iter() {
            control.cancellation().check()?;
            let operation = candidate
                .operation
                .map(|id| {
                    let name = directory::name(id.as_bytes());
                    directory::child(&repository.operations, directory::text(&name).as_ref())
                })
                .transpose()?;
            let _lease = if let Some(operation) = &operation {
                match directory::lock(
                    directory::open_file(operation, "lease", true)?,
                    true,
                    control,
                ) {
                    Ok(lease) => Some(lease),
                    // A resumed prepared operation may now own this lease. Its
                    // authenticated inventory was retained during discovery.
                    Err(BackupError::Busy) => continue,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            let directory = operation.as_ref().unwrap_or(&repository.objects);
            let name = directory::name(candidate.object);
            let file = directory::open_file(directory, directory::text(&name), false)?;
            if file.metadata()?.len() != candidate.bytes {
                return Err(BackupError::Integrity);
            }
            drop(file);
            #[cfg(test)]
            repository.before(super::super::Fault::CollectionUnlink)?;
            directory.remove_file(directory::text(&name))?;
            #[cfg(test)]
            repository.before(super::super::Fault::CollectionSync)?;
            directory::synchronize(directory)?;
            if operation.is_some() {
                add(&mut self.report.removed_staging_files, 1)?;
                add(&mut self.report.removed_staging_bytes, candidate.bytes)?;
            } else {
                add(&mut self.report.removed_objects, 1)?;
                add(&mut self.report.removed_object_bytes, candidate.bytes)?;
            }
            self.report.remaining_files -= 1;
        }
        Ok(())
    }
}

impl LocalRepository {
    /// Authenticate reachability outside repository admission, then revalidate
    /// its generation before at most `maximum_files` deletions. Retain whole
    /// packs referenced by completed/prepared roots or active captures. Private
    /// pack links from inactive operations are reclaimable; operation identities
    /// and durable cancellation/publication records remain intact.
    pub fn collect_garbage(
        &self,
        maximum_files: usize,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCollectionReport> {
        if maximum_files == 0 {
            return Err(BackupError::Invalid("zero collection deletion bound"));
        }
        let generation = self.retention_generation(control)?;
        let files = FileBudget::new(maximum_workspace_bytes);
        let discovered = self.discover_collection(maximum_files, &files, control);
        #[cfg(test)]
        if let Some(observe) = ON_DISCOVERY.with(|slot| slot.borrow_mut().take()) {
            observe();
        }
        let _admission = self.admission(control)?;
        if self.generation(control)? != generation {
            return Err(BackupError::Busy);
        }
        let mut collection = discovered?;
        if !collection.candidates.is_empty() {
            self.advance_removal_generation(control)?;
            collection.remove(self, control)?;
        }
        collection.report.workspace_peak_bytes = files.usage().1;
        Ok(collection.report)
    }

    fn discover_collection(
        &self,
        maximum: usize,
        files: &FileBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Collection> {
        let roots = self.scan_roots(files, control)?;
        let mut collection = Collection {
            reachable: Reachability::new(files, control),
            candidates: BudgetedVec::new(control.memory()),
            maximum,
            report: BackupCollectionReport {
                completed_backups: roots.len() as u64,
                ..BackupCollectionReport::default()
            },
        };
        for root in roots.iter() {
            control.cancellation().check()?;
            collection
                .reachable
                .retain_root(self, root?.1 .0, control)?;
        }
        collection.operations(self, &roots, control)?;
        collection.objects(self, control)?;
        Ok(collection)
    }
}

fn add(total: &mut u64, bytes: u64) -> BackupResult<()> {
    *total = total
        .checked_add(bytes)
        .ok_or(BackupError::Invalid("repository collection count overflow"))?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    pub(in crate::backup::repository) static ON_DISCOVERY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
