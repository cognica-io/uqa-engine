//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture role identities without loading unrelated relation or sequence definitions.

use super::persistence::{self, RoleCatalogSnapshot};
use crate::catalog::snapshot_read::with_read_transaction;
use std::sync::Arc;
use uqa_storage::{CatalogFacade, PersistentStorageSession, StorageBackendResult};

pub fn read_role_snapshot(
    current: RoleCatalogSnapshot,
    bound: Option<&dyn CatalogFacade>,
    independent: Option<&PersistentStorageSession>,
    preserve_private: bool,
) -> StorageBackendResult<RoleCatalogSnapshot> {
    let Some(independent) = independent else {
        return Ok(current);
    };
    with_read_transaction(independent, |catalog| {
        let restored = persistence::restore(catalog)?;
        let snapshot = RoleCatalogSnapshot {
            roles: Arc::new(restored.roles),
            memberships: Arc::new(restored.memberships),
        };
        if preserve_private {
            snapshot.merge_private(bound, &current)
        } else {
            Ok(snapshot)
        }
    })
}
