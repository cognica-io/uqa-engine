//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently versioned ACL replacements over a relation definition's security baseline.
//! Definition writes compact these records into their supplied security, atomically removing the replacements.

use super::{new_nonzero_catalog_identity, RelationIdentity, RelationSecurityRow};
use crate::{StorageBackendError, StorageBackendResult};
use serde::{Deserialize, Serialize};
use uqa_core::{
    catalog_acl::{BoundRelationSecurity, TablePrivileges},
    catalog_role::BoundAclEntry,
};

pub const METADATA_PREFIX: &str = "uqa.relation_acl.v1:";

pub fn prefix(relation: &RelationIdentity) -> String {
    format!(
        "{METADATA_PREFIX}{}:",
        serde_json::to_string(relation).expect("relation JSON")
    )
}

pub fn key(relation: &RelationIdentity, column: Option<&str>) -> String {
    format!(
        "{}{}",
        prefix(relation),
        serde_json::to_string(&column).expect("attribute JSON")
    )
}

pub fn column(prefix: &str, key: &str) -> StorageBackendResult<Option<String>> {
    let suffix = key.strip_prefix(prefix).ok_or_else(|| {
        StorageBackendError::Other("ACL tuple belongs to another relation".into())
    })?;
    let column: Option<String> = serde_json::from_str(suffix)?;
    if serde_json::to_string(&column)? != suffix {
        return Err(StorageBackendError::Other(
            "noncanonical ACL tuple key".into(),
        ));
    }
    Ok(column)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationAclTuple {
    format: u32,
    pub revision: [u8; 16],
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub acl: Option<Vec<BoundAclEntry<TablePrivileges>>>,
}

impl RelationAclTuple {
    pub fn new(acl: Option<Vec<BoundAclEntry<TablePrivileges>>>) -> StorageBackendResult<Self> {
        Ok(Self {
            format: 1,
            revision: new_nonzero_catalog_identity("relation", "ACL tuple")?,
            acl,
        })
    }

    pub fn decode(json: &[u8]) -> StorageBackendResult<Self> {
        let entry: Self = serde_json::from_slice(json)?;
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> StorageBackendResult<()> {
        if self.format != 1 || self.revision == [0; 16] {
            return Err(StorageBackendError::Other(
                "invalid relation ACL tuple identity or format".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn encode(&self, column: Option<&str>) -> StorageBackendResult<String> {
        self.validate()?;
        if column.is_some() && self.acl.is_none() {
            return Err(StorageBackendError::Other(
                "attribute ACL tuple has a null ACL".into(),
            ));
        }
        Ok(serde_json::to_string(self)?)
    }

    pub fn apply(
        &self,
        column: Option<&str>,
        security: &mut BoundRelationSecurity,
    ) -> StorageBackendResult<()> {
        self.validate()?;
        if let Some(column) = column {
            let acl = self.acl.as_ref().ok_or_else(|| {
                StorageBackendError::Other("attribute ACL tuple has a null ACL".into())
            })?;
            if acl.is_empty() {
                security.column_acls.remove(column);
            } else {
                security.column_acls.insert(column.to_owned(), acl.clone());
            }
            security
                .acl_revisions
                .columns
                .insert(column.to_owned(), self.revision);
        } else {
            security.acl.clone_from(&self.acl);
            security.acl_revisions.relation = Some(self.revision);
        }
        Ok(())
    }
}

/// One metadata scan serves every restored relation, avoiding a scan per table or column.
#[derive(Default)]
pub struct RelationAclRecords(
    std::collections::BTreeMap<RelationIdentity, Vec<(Option<String>, RelationAclTuple)>>,
);

impl RelationAclRecords {
    pub fn insert(&mut self, key: &str, value: &[u8]) -> StorageBackendResult<()> {
        let suffix = key
            .strip_prefix(METADATA_PREFIX)
            .ok_or_else(|| StorageBackendError::Other("invalid ACL metadata prefix".into()))?;
        let mut stream = serde_json::Deserializer::from_str(suffix).into_iter::<RelationIdentity>();
        let relation = stream
            .next()
            .ok_or_else(|| StorageBackendError::Other("missing ACL relation identity".into()))??;
        let prefix = prefix(&relation);
        let column = column(&prefix, key)?;
        self.0
            .entry(relation)
            .or_default()
            .push((column, RelationAclTuple::decode(value)?));
        Ok(())
    }

    pub fn apply(
        &self,
        relation: &RelationIdentity,
        security: &mut RelationSecurityRow,
    ) -> StorageBackendResult<()> {
        if let Some(tuples) = self.0.get(relation) {
            let RelationSecurityRow::Bound(security) = security else {
                return Err(StorageBackendError::Other(
                    "ACL tuple requires captured relation ownership".into(),
                ));
            };
            for (column, entry) in tuples {
                entry.apply(column.as_deref(), security)?;
            }
        }
        Ok(())
    }
}

/// Provider-serialized catalogs retain their embedded ACL format under the caller's existing transaction. Only versioned providers persist independently writable tuples.
pub(super) fn save_serialized<C: super::CatalogFacade + ?Sized>(
    catalog: &C,
    relation: &RelationIdentity,
    column: Option<&str>,
    entry: &RelationAclTuple,
) -> StorageBackendResult<()> {
    let apply = |security: &mut RelationSecurityRow| {
        let RelationSecurityRow::Bound(security) = security else {
            return Err(StorageBackendError::Other(
                "ACL replacement requires bound relation ownership".into(),
            ));
        };
        entry.apply(column, security)
    };
    if let Some(mut table) = catalog
        .load_tables()?
        .into_iter()
        .find(|table| &table.relation == relation)
    {
        apply(&mut table.security)?;
        return catalog.save_table(&table);
    }
    if let Some(mut view) = catalog
        .load_views()?
        .into_iter()
        .find(|view| &view.relation == relation)
    {
        apply(&mut view.security)?;
        return catalog.save_view(&view);
    }
    if let Some(mut table) = catalog
        .load_foreign_tables()?
        .into_iter()
        .find(|table| &table.relation == relation)
    {
        apply(&mut table.security)?;
        if catalog.update_foreign_table_security(relation, &table.security)? {
            return Ok(());
        }
    }
    Err(StorageBackendError::Other(
        "ACL relation disappeared before persistence".into(),
    ))
}

#[cfg(test)]
mod tests;
