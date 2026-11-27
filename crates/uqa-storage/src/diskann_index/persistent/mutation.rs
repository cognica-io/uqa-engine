//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication and pruning evaluate the real catalog, canonical input and generation head in one caller-owned write.

use super::{invalid, DiskANNBoundRead};
use crate::{
    diskann_index::{
        build::DiskANNCanonicalCoverage,
        catalog::{DiskANNIndexResolver, DiskANNIndexScope},
        changes::DiskANNPruneResult,
        format::DiskANNGeneration,
        pages::DiskANNPageSource,
    },
    key_value::{publication, KeyValueDiskANNPruner, KeyValueDiskANNSource, KeyValueRead},
    read_control::StorageReadControl,
    KeyValueBatch, StorageBackendResult,
};

/// Address physical generation records on the command's existing read and write batch.
pub trait DiskANNMutationView {
    fn with_generation_read<T>(
        &self,
        visit: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T>;
    fn with_generation_batch<T>(
        &self,
        batch: &mut dyn KeyValueBatch,
        visit: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T>;
}

impl DiskANNMutationView for dyn KeyValueRead + '_ {
    fn with_generation_read<T>(
        &self,
        visit: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        visit(self)
    }
    fn with_generation_batch<T>(
        &self,
        batch: &mut dyn KeyValueBatch,
        visit: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        visit(batch)
    }
}

/// A retained canonical source supplies its real catalog keys, canonical prefixes and population headers. Generation state transitions are identical in both record layouts.
pub trait DiskANNRetainedMutation<V: DiskANNMutationView + ?Sized>: DiskANNBoundRead {
    fn guard_catalog(
        &self,
        current: &V,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn with_canonical_records<T>(
        &self,
        current: &V,
        missing_owner: &'static str,
        control: &StorageReadControl,
        visit: impl FnOnce(&dyn KeyValueRead, &dyn KeyValueRead, &[&[u8]]) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T>;
    fn retire_population(
        &self,
        generation: DiskANNGeneration,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn publish_population(
        &self,
        sealed: &KeyValueDiskANNSource,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;

    /// Initialize an older selected generation's derived counts under its original catalog and head guards.
    fn initialize_population(
        &self,
        scope: &DiskANNIndexScope,
        selected: &KeyValueDiskANNSource,
        current: &V,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.guard_catalog(current, batch, control)?;
        current.with_generation_read(|read| {
            current.with_generation_batch(batch, |mapped| {
                publication::require_selected_generation(
                    scope,
                    read,
                    mapped,
                    selected.generation(),
                    control,
                )
            })
        })?;
        self.publish_population(selected, batch, control)
    }

    fn retire_generation(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        current: &V,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNGeneration> {
        self.guard_catalog(current, batch, control)?;
        let scope = self.index_scope(resolver, control)?;
        let generation = self.with_physical_read(|captured| {
            current.with_generation_read(|read| {
                current.with_generation_batch(batch, |mapped| {
                    publication::retire_captured_generation(&scope, captured, read, mapped, control)
                })
            })
        })?;
        self.retire_population(generation, batch, control)?;
        Ok(generation)
    }

    fn publish_generation(
        coverage: &DiskANNCanonicalCoverage<Self>,
        resolver: &dyn DiskANNIndexResolver,
        sealed: &KeyValueDiskANNSource,
        current: &V,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        coverage.check_control(control)?;
        let source = coverage.source();
        source.guard_catalog(current, batch, control)?;
        source.with_canonical_records(
            current,
            "missing canonical owner",
            control,
            |captured, current, prefixes| {
                let original = captured.revision(prefixes)?;
                if original.has_private_changes()
                    && !original.same_private_changes(&current.revision(prefixes)?)
                {
                    return Err(invalid(
                        "captured private canonical input changed before publication",
                    ));
                }
                Ok(())
            },
        )?;
        let scope = source.index_scope(resolver, control)?;
        let previous = source.with_physical_read(|captured| {
            current.with_generation_read(|read| {
                let previous = publication::selected_generation(&scope, captured, control)?;
                current.with_generation_batch(batch, |mapped| {
                    publication::publish_captured_generation(
                        coverage,
                        &scope,
                        source
                            .index_parameters()
                            .ok_or_else(|| invalid("missing index parameters"))?,
                        publication::DiskANNPublicationViews {
                            captured,
                            current: read,
                            batch: mapped,
                            sealed,
                        },
                        control,
                    )
                })?;
                Ok(previous)
            })
        })?;
        if let Some(previous) = previous {
            source.retire_population(previous, batch, control)?;
        }
        source.publish_population(sealed, batch, control)
    }

    /// Apply journal deletions only after the caller's batch has guarded the canonical input and selected generation.
    fn prune_journal(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        pruner: &KeyValueDiskANNPruner,
        current: &V,
        batch: &mut dyn KeyValueBatch,
        control: &StorageReadControl,
        prune_records: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<DiskANNPruneResult>,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        self.guard_catalog(current, batch, control)?;
        self.with_canonical_records(
            current,
            "pruning has no canonical owner",
            control,
            |_, read, prefixes| {
                if read.revision(prefixes)?.has_private_changes() {
                    return Err(invalid(
                        "journal pruning requires committed canonical input",
                    ));
                }
                Ok(())
            },
        )?;
        let scope = self.index_scope(resolver, control)?;
        current.with_generation_read(|read| {
            let parameters = self
                .index_parameters()
                .ok_or_else(|| invalid("missing index parameters"))?;
            current.with_generation_batch(batch, |mapped| {
                pruner.require_selected(
                    &scope,
                    self.dimensions(),
                    parameters,
                    read,
                    mapped,
                    control,
                )
            })
        })?;
        let result = prune_records(batch)?;
        self.check_control(control)?;
        Ok(result)
    }
}
