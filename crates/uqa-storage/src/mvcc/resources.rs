//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical build and retained-reader lifetimes, independent of SQL SSI participation.

use std::sync::{Arc, Weak};

use uqa_core::memory::Budgeted;

use super::{
    DatabaseId, SerializableLeases, SerializableTransactionId, StorageTransactionId, VersionError,
    VersionResult,
};
use crate::read_control::StorageReadControl;

/// A never-reused allocation in the database-wide resource namespace, scoped to one transaction-history incarnation. The upper bit is reserved for recovery exclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceLeaseId(StorageTransactionId);

impl ResourceLeaseId {
    pub fn new(database: DatabaseId, allocation: u64) -> VersionResult<Self> {
        if allocation > u64::MAX / 2 {
            return Err(VersionError::InvalidTransactionId);
        }
        StorageTransactionId::new(database, allocation).map(Self)
    }

    pub const fn database(self) -> DatabaseId {
        self.0.database()
    }

    pub const fn allocation(self) -> u64 {
        self.0.allocation()
    }
}

/// Claim starts a build only when nobody retains it; Share retains immutable data from an existing or abandoned build. Recover excludes both kinds until the recovery operation and any uncertain attempt finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceLeaseRequest {
    Claim,
    Share,
    Recover,
}

impl ResourceLeaseId {
    pub fn tag(self, request: ResourceLeaseRequest) -> u64 {
        self.allocation() * 2 + u64::from(request == ResourceLeaseRequest::Recover)
    }
}

/// Clones retain the original physical resource owner. Final release performs no transaction or recovery operation.
#[derive(Clone)]
pub struct ResourceLease {
    id: ResourceLeaseId,
    retained: Arc<dyn Send + Sync>,
}

impl ResourceLease {
    /// Providers call this only while holding authoritative admission. The retained value must keep the physical database and native/local liveness owner alive.
    pub fn retain<T: Send + Sync + 'static>(
        id: ResourceLeaseId,
        retained: T,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.check()?;
        Ok(Self {
            id,
            retained: Budgeted::new(retained, control.memory().empty_reservation())
                .into_shared()?,
        })
    }

    pub fn id(&self) -> ResourceLeaseId {
        self.id
    }

    pub(crate) fn downgrade(&self) -> WeakResourceLease {
        WeakResourceLease {
            id: self.id,
            retained: Arc::downgrade(&self.retained),
        }
    }
}

pub(crate) struct WeakResourceLease {
    id: ResourceLeaseId,
    retained: Weak<dyn Send + Sync>,
}

impl WeakResourceLease {
    pub(crate) fn id(&self) -> ResourceLeaseId {
        self.id
    }

    pub(crate) fn upgrade(&self) -> Option<ResourceLease> {
        Some(ResourceLease {
            id: self.id,
            retained: self.retained.upgrade()?,
        })
    }
}

pub trait ResourceLeaseProvider: Send + Sync {
    /// Check the requested claim/share/recovery exclusion and retain the physical owner under one provider admission. `None` means a conflicting owner is alive. Process death releases native leases; age, a missing catalog head and an unknown commit result never prove abandonment. No admission lock remains held by the returned handle.
    fn try_acquire_resource(
        &self,
        id: ResourceLeaseId,
        request: ResourceLeaseRequest,
        control: &StorageReadControl,
    ) -> VersionResult<Option<ResourceLease>>;
}

/// Reuse local lease transport under its caller-held admission, in a namespace distinct from receipts and SSI. This neither admits an SSI participant nor modifies a transaction graph. Native transports that ignore the coordinator namespace must use a separate physical lease file instead.
pub fn retain_local_resource(
    leases: &dyn SerializableLeases,
    id: ResourceLeaseId,
    request: ResourceLeaseRequest,
    control: &StorageReadControl,
) -> VersionResult<Option<ResourceLease>> {
    let participant = |request| {
        SerializableTransactionId::new(id.database(), *b"UQAResourceLease", id.tag(request))
    };
    if leases.is_alive(participant(ResourceLeaseRequest::Recover)?, control)?
        || (request != ResourceLeaseRequest::Share
            && leases.is_alive(participant(ResourceLeaseRequest::Share)?, control)?)
    {
        return Ok(None);
    }
    let retained = if request == ResourceLeaseRequest::Share {
        leases.retain_shared(participant(request)?, control)?
    } else {
        leases.retain(participant(request)?, control)?
    };
    if retained.id() != participant(request)? {
        return Err(VersionError::InvalidEncoding(
            "resource transport returned another owner",
        ));
    }
    ResourceLease::retain(id, retained, control).map(Some)
}
