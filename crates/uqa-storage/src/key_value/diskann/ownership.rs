//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained builds and uncertain original attempts keep physical ownership alive.

use crate::mvcc::{
    IdentifierRequest, ResourceLease, ResourceLeaseId, ResourceLeaseRequest, VersionError,
};
use crate::read_control::StorageReadControl;
use crate::StorageBackendResult;

use super::state::StageOwner;
use super::{invalid, KeyValueDiskANNStore};

impl KeyValueDiskANNStore {
    pub(super) fn reserve_owner(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ResourceLease> {
        // One namespace per history database, independent of table/index retirement. Its watermark is never removed or rolled back.
        let allocation = self
            .owner
            .store
            .identifier_allocator()
            .ok_or_else(|| invalid("durable resource identifiers are unavailable"))?
            .allocate_identifiers(
                b"\0uqa-resource-owners-v1\0",
                IdentifierRequest::Reserve {
                    minimum: 2,
                    maximum: u64::MAX / 2,
                    count: std::num::NonZeroU64::MIN,
                },
            )?;
        self.acquire_owner(
            StageOwner::Leased(allocation.watermark()),
            ResourceLeaseRequest::Claim,
            control,
        )?
        .ok_or_else(|| invalid("new staging owner allocation is already live"))
    }

    /// Allocation one is permanently reserved for legacy-state transitions. Before their first revision-2 commit, the new per-build allocation is not visible to another recovery owner.
    pub(super) fn legacy_guard(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<ResourceLease>> {
        self.acquire_owner(
            StageOwner::Leased(1),
            ResourceLeaseRequest::Recover,
            control,
        )
    }

    pub(super) fn retain_transition(
        lease: &ResourceLease,
        legacy: ResourceLease,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ResourceLease> {
        ResourceLease::retain(lease.id(), (lease.clone(), legacy), control)
            .map_err(VersionError::into_storage_error)
    }

    /// Ordinary handles may share their original repository's owner. Recovery never joins an existing owner, including one in this repository.
    pub(super) fn acquire_owner(
        &self,
        owner: StageOwner,
        request: ResourceLeaseRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<ResourceLease>> {
        control.check()?;
        let StageOwner::Leased(allocation) = owner else {
            return Err(invalid("legacy staging owner requires an atomic upgrade"));
        };
        let id = ResourceLeaseId::new(self.owner.database, allocation)
            .map_err(VersionError::into_storage_error)?;
        let mut owners = self.owner.leases.lock();
        let mut kept = 0;
        for index in 0..owners.len() {
            if owners[index].upgrade().is_some() {
                owners.swap(kept, index);
                kept += 1;
            }
        }
        owners.truncate(kept);
        if request != ResourceLeaseRequest::Recover {
            if let Some(lease) = owners
                .iter()
                .find(|owner| owner.id() == id)
                .and_then(crate::mvcc::WeakResourceLease::upgrade)
            {
                return Ok(Some(lease));
            }
        }
        let lease = self
            .owner
            .store
            .resource_leases()
            .ok_or_else(|| invalid("physical resource leases are unavailable"))?
            .try_acquire_resource(id, request, control)
            .map_err(VersionError::into_storage_error)?;
        if lease.as_ref().is_some_and(|lease| lease.id() != id) {
            return Err(invalid("provider returned another resource owner"));
        }
        if request != ResourceLeaseRequest::Recover {
            if let Some(lease) = &lease {
                owners.push(lease.downgrade())?;
            }
        }
        Ok(lease)
    }
}
