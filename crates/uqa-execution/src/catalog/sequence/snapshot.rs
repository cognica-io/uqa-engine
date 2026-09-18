//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain sequence definitions and authorization from one committed view plus private records.

use super::{
    restoration::{
        load_sequence_value_rows, prepare_sequence_rows, select_sequence_records,
        RestoredSequenceRegistry,
    },
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

enum SequenceSource {
    Current,
    Committed,
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
    pub fn named_states(&self) -> BTreeMap<String, SequenceState> {
        self.sequences
            .iter()
            .map(|(relation, state)| (relation.qualified_name(), *state))
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = self
            .sequences
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Select from SQL's ordered search-path candidates without publishing the detached metadata into live registries.
    pub fn first_state(&self, candidates: &[RelationIdentity]) -> Option<(String, SequenceState)> {
        candidates.iter().find_map(|relation| {
            self.sequences
                .get(relation)
                .map(|state| (relation.qualified_name(), *state))
        })
    }

    /// Keep the latest coherent definitions and roles while retaining complete private sequence records and session-local temporary entries. No catalog rows are reloaded from another committed view.
    pub fn merge_private(
        mut self,
        catalog: Option<&dyn CatalogFacade>,
        current: &Self,
    ) -> StorageBackendResult<Self> {
        self.roles = self.roles.merge_private(catalog, &current.roles)?;
        self.merge_sequence_records(current, |relation, object_id| {
            catalog.map_or(Ok(false), |catalog| {
                catalog.sequence_has_private_changes(relation, object_id)
            })
        })
    }

    fn merge_sequence_records(
        mut self,
        current: &Self,
        mut private: impl FnMut(&RelationIdentity, [u8; 16]) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Self> {
        let selected = select_sequence_records(
            current.object_ids.iter().map(|(relation, object_id)| {
                (relation.clone(), (SequenceSource::Current, *object_id))
            }),
            self.object_ids.iter().map(|(relation, object_id)| {
                (relation.clone(), (SequenceSource::Committed, *object_id))
            }),
            |relation, (source, object_id)| {
                let snapshot = match source {
                    SequenceSource::Current => current,
                    SequenceSource::Committed => &self,
                };
                if snapshot.persistence.get(relation) == Some(&RelationPersistence::Temporary) {
                    return Ok(true);
                }
                private(relation, *object_id)
            },
        )?;
        let removed = self
            .object_ids
            .keys()
            .filter(|relation| !selected.contains_key(*relation))
            .cloned()
            .collect::<Vec<_>>();
        for relation in removed {
            Arc::make_mut(&mut self.sequences).remove(&relation);
            Arc::make_mut(&mut self.object_ids).remove(&relation);
            Arc::make_mut(&mut self.persistence).remove(&relation);
            Arc::make_mut(&mut self.security).remove(&relation);
        }
        for (relation, (source, object_id)) in selected {
            if matches!(source, SequenceSource::Committed) {
                continue;
            }
            let (Some(state), Some(persistence), Some(security)) = (
                current.sequences.get(&relation),
                current.persistence.get(&relation),
                current.security.get(&relation),
            ) else {
                return Err(StorageBackendError::Other(format!(
                    "sequence `{}` has incomplete private catalog metadata",
                    relation.qualified_name()
                )));
            };
            Arc::make_mut(&mut self.sequences).insert(relation.clone(), *state);
            Arc::make_mut(&mut self.object_ids).insert(relation.clone(), object_id);
            Arc::make_mut(&mut self.persistence).insert(relation.clone(), *persistence);
            Arc::make_mut(&mut self.security).insert(relation, security.clone());
        }
        Ok(self)
    }

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
