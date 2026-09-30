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
use crate::catalog::{
    security::roles::persistence::{self, RoleCatalogSnapshot},
    snapshot_read::with_read_transaction,
};
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
            BoundSequenceSecurity,
        },
    },
};
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

#[derive(Clone)]
pub struct SequenceReadSnapshot {
    pub sequences: Arc<BTreeMap<RelationIdentity, SequenceState>>,
    pub object_ids: Arc<BTreeMap<RelationIdentity, [u8; 16]>>,
    pub persistence: Arc<BTreeMap<RelationIdentity, RelationPersistence>>,
    pub security: Arc<BTreeMap<RelationIdentity, BoundSequenceSecurity>>,
    /// The `pg_class` OIDs sequences recorded when they were created, by object identity.
    pub catalog_oids: Arc<BTreeMap<[u8; 16], u32>>,
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
            if let Some(oid) = current.catalog_oids.get(&object_id) {
                Arc::make_mut(&mut self.catalog_oids).insert(object_id, *oid);
            }
            Arc::make_mut(&mut self.persistence).insert(relation.clone(), *persistence);
            Arc::make_mut(&mut self.security).insert(relation, security.clone());
        }
        Ok(self)
    }

    /// The OID of the sequence with the object identity.
    pub fn catalog_oid(&self, object_id: &[u8; 16]) -> i64 {
        super::catalog_oids::sequence_catalog_oid(&self.catalog_oids, object_id)
    }

    /// The sequence with the OID.
    pub fn relation_with_oid(&self, oid: i64) -> Option<RelationIdentity> {
        self.object_ids.iter().find_map(|(relation, object_id)| {
            (self.catalog_oid(object_id) == oid).then(|| relation.clone())
        })
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

    /// Replace the persistent sequences with the catalog's rows and recorded OIDs, keeping the session's temporary ones.
    fn with_rows(
        self,
        rows: Vec<uqa_storage::SequenceRow>,
        recorded: BTreeMap<[u8; 16], u32>,
    ) -> StorageBackendResult<Self> {
        let temporary = RestoredSequenceRegistry::temporary(
            &self.sequences,
            &self.object_ids,
            &self.persistence,
            &self.security,
        );
        let registry = prepare_sequence_rows(temporary, rows, &self.roles.roles)?;
        let catalog_oids = registry
            .object_ids
            .values()
            .filter_map(|object_id| {
                recorded
                    .get(object_id)
                    .or_else(|| self.catalog_oids.get(object_id))
                    .map(|oid| (*object_id, *oid))
            })
            .collect();
        Ok(Self {
            sequences: Arc::new(registry.sequences),
            object_ids: Arc::new(registry.object_ids),
            persistence: Arc::new(registry.persistence),
            security: Arc::new(registry.security),
            catalog_oids: Arc::new(catalog_oids),
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
            Some(catalog) => current.with_rows(
                catalog.load_sequence_rows()?,
                super::catalog_oids::load(catalog)?,
            ),
            None => Ok(current),
        };
    };
    with_read_transaction(independent, |catalog| {
        let roles = persistence::restore(catalog)?;
        let roles = RoleCatalogSnapshot {
            roles: Arc::new(roles.roles),
            memberships: Arc::new(roles.memberships),
        };
        let (rows, recorded) = if preserve_private {
            current.roles = roles.merge_private(bound, &current.roles)?;
            match bound {
                Some(bound) => (
                    load_sequence_value_rows(bound, catalog)?,
                    super::catalog_oids::load(bound)?,
                ),
                None => (
                    catalog.load_sequence_rows()?,
                    super::catalog_oids::load(catalog)?,
                ),
            }
        } else {
            current.roles = roles;
            (
                catalog.load_sequence_rows()?,
                super::catalog_oids::load(catalog)?,
            )
        };
        current.with_rows(rows, recorded)
    })
}

#[cfg(test)]
mod tests;
