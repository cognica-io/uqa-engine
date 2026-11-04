//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain sequence definitions and authorization from one committed view plus private records.

use super::{
    registry::SequenceCatalog,
    restoration::{load_sequence_value_rows, prepare_sequence_rows, select_sequence_records},
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
        security::sequence_inquiry::{
            SequencePrivilegeInquiry, SequenceSecurityCatalog, SequenceSecurityRead,
        },
    },
};
use uqa_storage::{
    CatalogFacade, PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

#[derive(Clone)]
pub struct SequenceReadSnapshot {
    pub catalog: Arc<SequenceCatalog>,
    pub roles: RoleCatalogSnapshot,
}

pub trait SequenceSnapshotSource {
    /// Sequence definitions with the value state of their durable records, which may run ahead of the values handed out.
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot>;

    /// The exact position recorded for one allocation generation of a sequence, if its durable record runs ahead.
    fn sequence_position(
        &self,
        _key: crate::row_locks::SequencePositionKey,
    ) -> StorageBackendResult<Option<crate::row_locks::RecordedSequencePosition>> {
        Ok(None)
    }
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
        Box::new(self.catalog.security())
    }
}

impl SequenceReadSnapshot {
    pub fn states_at_positions(
        &self,
        positions: Option<
            &std::collections::HashMap<
                crate::row_locks::SequencePositionKey,
                crate::row_locks::RecordedSequencePosition,
            >,
        >,
    ) -> BTreeMap<RelationIdentity, SequenceState> {
        self.catalog
            .iter()
            .map(|(relation, record)| {
                let position = positions
                    .and_then(|positions| {
                        positions.get(&record.state.position_key(record.object_id))
                    })
                    .copied();
                (relation.clone(), record.state.at_position(position))
            })
            .collect()
    }

    pub fn named_states(&self) -> BTreeMap<String, SequenceState> {
        self.catalog
            .states()
            .map(|(relation, state)| (relation.qualified_name(), *state))
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = self
            .catalog
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Select from SQL's ordered search-path candidates without publishing the detached metadata into live registries.
    pub fn first_state(&self, candidates: &[RelationIdentity]) -> Option<(String, SequenceState)> {
        candidates.iter().find_map(|relation| {
            self.catalog
                .state(relation)
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
            current.catalog.object_ids().map(|(relation, object_id)| {
                (relation.clone(), (SequenceSource::Current, *object_id))
            }),
            self.catalog.object_ids().map(|(relation, object_id)| {
                (relation.clone(), (SequenceSource::Committed, *object_id))
            }),
            |relation, (source, object_id)| {
                let snapshot = match source {
                    SequenceSource::Current => current,
                    SequenceSource::Committed => &self,
                };
                if snapshot.catalog.persistence(relation) == Some(RelationPersistence::Temporary) {
                    return Ok(true);
                }
                private(relation, *object_id)
            },
        )?;
        let removed = self
            .catalog
            .keys()
            .filter(|relation| !selected.contains_key(*relation))
            .cloned()
            .collect::<Vec<_>>();
        for relation in removed {
            Arc::make_mut(&mut self.catalog)
                .remove_selected(&relation)
                .map_err(|error| StorageBackendError::backend("select private sequence", error))?;
        }
        for (relation, (source, _)) in selected {
            if matches!(source, SequenceSource::Committed) {
                continue;
            }
            let record = *current
                .catalog
                .get(&relation)
                .expect("selected complete sequence exists");
            let security = current.catalog.security()[&relation].clone();
            Arc::make_mut(&mut self.catalog)
                .insert(relation, record, security)
                .map_err(|error| StorageBackendError::backend("select private sequence", error))?;
        }
        Ok(self)
    }

    /// The OID of the sequence with the object identity.
    pub fn catalog_oid(&self, object_id: &[u8; 16]) -> i64 {
        self.catalog.catalog_oid(object_id)
    }

    /// The sequence with the OID.
    pub fn relation_with_oid(&self, oid: i64) -> Option<RelationIdentity> {
        self.catalog.object_ids().find_map(|(relation, object_id)| {
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
        let temporary = self.catalog.temporary_only();
        let mut registry = prepare_sequence_rows(temporary, rows, &self.roles.roles)?;
        let catalog_oids = registry
            .object_ids()
            .filter_map(|(_, object_id)| {
                recorded
                    .get(object_id)
                    .or_else(|| self.catalog.catalog_oids().get(object_id))
                    .map(|oid| (*object_id, *oid))
            })
            .collect::<Vec<_>>();
        registry
            .replace_catalog_oids(catalog_oids)
            .map_err(|error| StorageBackendError::backend("load sequence OIDs", error))?;
        Ok(Self {
            catalog: Arc::new(registry),
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
