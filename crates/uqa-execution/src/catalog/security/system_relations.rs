//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore and merge independently persisted relation and attribute ACL tuples.

use std::ops::DerefMut;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::{
    security::system_relations::{
        metadata_key, validate_security, SystemAcl, SystemRelationSecurities,
        SystemRelationSecurityCatalog, METADATA_PREFIX,
    },
    SystemRelation,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod persistence;

pub trait SystemRelationSecurityState: SystemRelationSecurityCatalog {
    fn system_relation_securities_write(
        &self,
    ) -> Box<dyn DerefMut<Target = SystemRelationSecurities> + '_>;
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
    allow_migration: bool,
) -> StorageBackendResult<SystemRelationSecurities> {
    let mut result = SystemRelationSecurities::new();
    let mut migrations = Vec::new();
    for (key, json) in catalog.metadata_with_prefix(METADATA_PREFIX)? {
        let (name, column) = key
            .strip_prefix(METADATA_PREFIX)
            .and_then(|suffix| suffix.split_once(':'))
            .ok_or_else(|| StorageBackendError::Other("invalid system ACL metadata key".into()))?;
        let relation = SystemRelation::from_qualified_name(name)
            .ok_or_else(|| StorageBackendError::Other("unknown system ACL relation".into()))?;
        let (entry, migrated) = persistence::decode(&json, roles, allow_migration)?;
        if entry.revision == [0; 16] {
            return Err(StorageBackendError::Other(
                "invalid system ACL tuple identity".into(),
            ));
        }
        if migrated {
            migrations.push((key.clone(), persistence::encode(&entry)?));
        }
        let identity = RelationIdentity::new(relation.namespace(), relation.name());
        let mut security = result.get(&identity).cloned().unwrap_or_default();
        if column.is_empty() {
            security.table = Some(entry);
        } else if relation.column_names().iter().any(|name| name == column) {
            security.columns.insert(column.to_string(), entry);
        } else {
            return Err(StorageBackendError::Other(
                "unknown system ACL attribute".into(),
            ));
        }
        result
            .insert(identity, security)
            .map_err(|error| StorageBackendError::backend("system relation ACL", error))?;
    }
    for (identity, entry) in &result {
        let relation = SystemRelation::at(&identity.schema, &identity.name)
            .expect("validated system identity");
        let security = entry
            .security(relation)
            .resolve(roles)
            .map_err(StorageBackendError::Other)?;
        validate_security(relation, &security, roles).map_err(StorageBackendError::Other)?;
    }
    for (key, value) in migrations {
        catalog.set_metadata(&key, &value)?;
    }
    Ok(result)
}

pub struct SystemPrivilegeUpdate {
    pub relation: SystemRelation,
    pub column: Option<String>,
    pub entry: SystemAcl,
}
impl SystemPrivilegeUpdate {
    pub fn new(
        relation: SystemRelation,
        column: Option<String>,
        acl: Vec<uqa_sql::catalog::security::table_binding::BoundTableAclEntry>,
    ) -> StorageBackendResult<Self> {
        let revision = crate::catalog::identity::new_nonzero_catalog_identity(
            &relation.qualified_name(),
            "ACL tuple",
        )?;
        Ok(Self {
            relation,
            column,
            entry: SystemAcl { revision, acl },
        })
    }
    pub fn persist(&self, catalog: Option<&dyn CatalogFacade>) -> StorageBackendResult<()> {
        if let Some(catalog) = catalog {
            catalog.set_metadata(
                &metadata_key(self.relation, self.column.as_deref()),
                &persistence::encode(&self.entry)?,
            )?;
        }
        Ok(())
    }
    pub fn publish(
        &self,
        securities: &mut SystemRelationSecurities,
    ) -> Result<(), uqa_sql::SQLError> {
        let identity = RelationIdentity::new(self.relation.namespace(), self.relation.name());
        let mut security = securities.get(&identity).cloned().unwrap_or_default();
        if let Some(column) = &self.column {
            security.columns.insert(column.clone(), self.entry.clone());
        } else {
            security.table = Some(self.entry.clone());
        }
        securities.insert(identity, security)
    }
}

/// Fixed data snapshots still refresh catalogs. Only records actually written by this transaction overlay that latest catalog, including empty attribute ACL replacements after REVOKE.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &SystemRelationSecurities,
    mut latest: SystemRelationSecurities,
) -> StorageBackendResult<SystemRelationSecurities> {
    let Some(catalog) = catalog else {
        return Ok(latest);
    };
    for (identity, security) in current {
        let relation = SystemRelation::at(&identity.schema, &identity.name).ok_or_else(|| {
            StorageBackendError::Other("non-system relation in system ACL registry".into())
        })?;
        let mut changed = None;
        if security.table.is_some()
            && catalog.metadata_has_private_changes(&metadata_key(relation, None))?
        {
            changed
                .get_or_insert_with(|| latest.get(identity).cloned().unwrap_or_default())
                .table
                .clone_from(&security.table);
        }
        for (column, entry) in &security.columns {
            if catalog.metadata_has_private_changes(&metadata_key(relation, Some(column)))? {
                changed
                    .get_or_insert_with(|| latest.get(identity).cloned().unwrap_or_default())
                    .columns
                    .insert(column.clone(), entry.clone());
            }
        }
        if let Some(changed) = changed {
            latest
                .insert(identity.clone(), changed)
                .map_err(|error| StorageBackendError::backend("system relation ACL", error))?;
        }
    }
    Ok(latest)
}

pub fn tuple_revision(
    registry: &dyn SystemRelationSecurityCatalog,
    relation: SystemRelation,
    column: Option<&str>,
) -> Option<[u8; 16]> {
    registry
        .system_relation_securities()
        .get(&RelationIdentity::new(
            relation.namespace(),
            relation.name(),
        ))
        .and_then(|security| security.entry(column))
        .map(|entry| entry.revision)
}

#[cfg(test)]
mod tests;
