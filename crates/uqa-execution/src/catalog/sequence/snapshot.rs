//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain sequence definitions and authorization from one committed view plus private records.

use super::{
    restoration::{load_sequence_value_rows, prepare_sequence_rows, RestoredSequenceRegistry},
    SequenceState,
};
use crate::catalog::security::roles::persistence::{self, RoleCatalogSnapshot};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{
        roles::guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
        security::{
            sequence_inquiry::{
                SequencePrivilegeInquiry, SequenceSecurityCatalog, SequenceSecurityRead,
            },
            SequenceSecurity,
        },
    },
};
use uqa_storage::{
    CatalogFacade, PersistentStorageBackend, PersistentStorageSession, StorageBackendError,
    StorageBackendResult,
};

#[derive(Clone)]
pub struct SequenceReadSnapshot {
    pub sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub persistence: Arc<BTreeMap<RelationIdentity, RelationPersistence>>,
    pub security: Arc<BTreeMap<RelationIdentity, SequenceSecurity>>,
    pub roles: RoleCatalogSnapshot,
}

pub trait SequenceSnapshotSource {
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot>;
}

impl RoleCatalogGuards for SequenceReadSnapshot {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(self.roles.roles.as_ref())
    }

    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(self.roles.memberships.as_ref())
    }
}

impl SequenceSecurityCatalog for SequenceReadSnapshot {
    fn security_read(&self) -> SequenceSecurityRead<'_> {
        Box::new(self.security.as_ref())
    }
}

impl SequenceReadSnapshot {
    pub fn privileges<'a>(
        &'a self,
        inquiry: &'a SequencePrivilegeInquiry<'_>,
    ) -> SequencePrivilegeInquiry<'a> {
        SequencePrivilegeInquiry {
            names: inquiry.names,
            roles: self,
            security: self,
            resolution: inquiry.resolution,
        }
    }

    fn with_rows(self, rows: Vec<uqa_storage::SequenceRow>) -> StorageBackendResult<Self> {
        let temporary = RestoredSequenceRegistry::temporary(
            &self.sequences,
            &self.object_ids,
            &self.persistence,
            &self.security,
        );
        let registry = prepare_sequence_rows(temporary, rows)?;
        Ok(Self {
            sequences: Arc::new(registry.sequences),
            object_ids: Arc::new(registry.object_ids),
            persistence: Arc::new(registry.persistence),
            security: Arc::new(registry.security),
            roles: self.roles,
        })
    }
}

/// The independent session is used only for reads and released before a value reservation begins. The bound transaction keeps its ordinary row snapshot and private metadata.
pub fn read_sequence_snapshot(
    mut current: SequenceReadSnapshot,
    bound: Option<&dyn CatalogFacade>,
    independent: Option<&PersistentStorageSession>,
    preserve_private: bool,
) -> StorageBackendResult<SequenceReadSnapshot> {
    let Some(independent) = independent else {
        return match bound {
            Some(catalog) => current.with_rows(catalog.load_sequence_rows()?),
            None => Ok(current),
        };
    };
    with_read_transaction(independent, |catalog| {
        let roles = persistence::restore(catalog)?;
        let roles = RoleCatalogSnapshot {
            roles: Arc::new(roles.roles),
            memberships: Arc::new(roles.memberships),
        };
        let rows = if preserve_private {
            current.roles = roles.merge_private(bound, &current.roles)?;
            match bound {
                Some(bound) => load_sequence_value_rows(bound, catalog)?,
                None => catalog.load_sequence_rows()?,
            }
        } else {
            current.roles = roles;
            catalog.load_sequence_rows()?
        };
        current.with_rows(rows)
    })
}

fn with_read_transaction<T>(
    session: &PersistentStorageSession,
    read: impl FnOnce(&dyn CatalogFacade) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    session.validate_transaction_affinity()?;
    if session.backend.in_transaction() {
        return Err(StorageBackendError::Other(
            "sequence snapshot reads require an idle independent session".into(),
        ));
    }
    session.backend.begin_read_transaction()?;
    let mut transaction = ReadTransaction {
        backend: session.backend.as_ref(),
        active: true,
    };
    let result = session
        .backend
        .pin_transaction_snapshot()
        .and_then(|()| read(session.catalog.as_ref()));
    session.backend.rollback_transaction()?;
    transaction.active = false;
    result
}

struct ReadTransaction<'a> {
    backend: &'a dyn PersistentStorageBackend,
    active: bool,
}

impl Drop for ReadTransaction<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.backend.rollback_transaction();
        }
    }
}

#[cfg(test)]
mod tests;
