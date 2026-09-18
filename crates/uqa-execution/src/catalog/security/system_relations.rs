//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore and merge independently persisted relation and attribute ACL tuples.

use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::{
        system_relations::{
            metadata_key, validate_security, SystemAcl, SystemRelationSecurities,
            SystemRelationSecurityCatalog, METADATA_PREFIX,
        },
        TableSecurity,
    },
    SystemRelation,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub trait SystemRelationSecurityState: SystemRelationSecurityCatalog {
    fn system_relation_securities_write(
        &self,
    ) -> Box<dyn DerefMut<Target = SystemRelationSecurities> + '_>;
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<SystemRelationSecurities> {
    let mut result = SystemRelationSecurities::new();
    for (key, json) in catalog.metadata_with_prefix(METADATA_PREFIX)? {
        let (name, column) = key
            .strip_prefix(METADATA_PREFIX)
            .and_then(|suffix| suffix.split_once(':'))
            .ok_or_else(|| StorageBackendError::Other("invalid system ACL metadata key".into()))?;
        let relation = SystemRelation::from_qualified_name(name)
            .ok_or_else(|| StorageBackendError::Other("unknown system ACL relation".into()))?;
        let entry: SystemAcl = serde_json::from_str(&json)?;
        if entry.revision == [0; 16] {
            return Err(StorageBackendError::Other(
                "invalid system ACL tuple identity".into(),
            ));
        }
        let security = result
            .entry(RelationIdentity::new(relation.namespace(), relation.name()))
            .or_default();
        if column.is_empty() {
            security.table = Some(entry);
        } else if relation.column_names().iter().any(|name| name == column) {
            security.columns.insert(column.to_string(), entry);
        } else {
            return Err(StorageBackendError::Other(
                "unknown system ACL attribute".into(),
            ));
        }
    }
    for (identity, entry) in &result {
        let relation = SystemRelation::at(&identity.schema, &identity.name)
            .expect("validated system identity");
        validate_security(relation, &entry.security(relation), roles)
            .map_err(StorageBackendError::Other)?;
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
        acl: Vec<uqa_sql::catalog::security::TableAclEntry>,
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
                &serde_json::to_string(&self.entry)?,
            )?;
        }
        Ok(())
    }
    pub fn publish(self, securities: &mut SystemRelationSecurities) {
        let security = securities
            .entry(RelationIdentity::new(
                self.relation.namespace(),
                self.relation.name(),
            ))
            .or_default();
        if let Some(column) = self.column {
            security.columns.insert(column, self.entry);
        } else {
            security.table = Some(self.entry);
        }
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
        if security.table.is_some()
            && catalog.metadata_has_private_changes(&metadata_key(relation, None))?
        {
            latest
                .entry(identity.clone())
                .or_default()
                .table
                .clone_from(&security.table);
        }
        for (column, entry) in &security.columns {
            if catalog.metadata_has_private_changes(&metadata_key(relation, Some(column)))? {
                latest
                    .entry(identity.clone())
                    .or_default()
                    .columns
                    .insert(column.clone(), entry.clone());
            }
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

pub fn changed_columns<'a>(
    before: &'a TableSecurity,
    after: &'a TableSecurity,
) -> impl Iterator<Item = &'a String> {
    before
        .column_acls
        .keys()
        .chain(after.column_acls.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|name| before.column_acls.get(*name) != after.column_acls.get(*name))
}

#[cfg(test)]
mod tests;
