//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{ColumnType, CreateDomain};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use super::roles::{identity::RoleSubject, RoleDefinition, RoleReference};

pub fn domain_object_oid(object_id: &[u8; 16]) -> u32 {
    u32::try_from(super::oids::stable_object_oid("domain", object_id))
        .expect("catalog OIDs fit in u32")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDomain<Owner = RoleIdentity> {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub identity: RelationIdentity,
    pub owner: Owner,
    pub definition: CreateDomain,
}

impl<Owner> StoredDomain<Owner> {
    pub fn column_type(&self) -> ColumnType {
        ColumnType::Domain {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            base: Box::new(self.definition.base.clone()),
        }
    }
}

impl StoredDomain<String> {
    /// Bind a legacy name only when the catalog restoration owner allows conversion.
    pub fn bind_owner(
        self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<StoredDomain, String> {
        let owner = RoleReference::Named(self.owner)
            .bind(roles)
            .map_err(|error| error.to_string())?
            .identity();
        Ok(StoredDomain {
            object_id: self.object_id,
            oid: self.oid,
            identity: self.identity,
            owner,
            definition: self.definition,
        })
    }
}

/// Validate the complete catalog before any durable conversion or registry publication.
pub fn validate_domain_registry(
    registry: &BTreeMap<String, StoredDomain>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    validate_domain_definitions(registry, roles)?;
    for domain in registry.values() {
        crate::schema::domains::constraints::validate(&domain.definition, false)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Early restoration validates authority and supplied identities before any missing legacy constraint metadata is allocated.
pub fn validate_domain_definitions(
    registry: &BTreeMap<String, StoredDomain>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    let mut constraint_objects = BTreeSet::new();
    let mut constraint_oids = BTreeSet::new();
    for (name, domain) in registry {
        if domain.object_id == [0; 16]
            || !identities.insert(domain.object_id)
            || domain.oid != domain_object_oid(&domain.object_id)
            || !oids.insert(domain.oid)
        {
            return Err(format!("invalid or duplicate domain identity for `{name}`"));
        }
        if domain.identity.schema.is_empty()
            || domain.identity.name.is_empty()
            || domain.identity.qualified_name() != *name
            || RelationIdentity::from_legacy_name(&domain.definition.name).as_ref()
                != Ok(&domain.identity)
        {
            return Err(format!("inconsistent domain name for `{name}`"));
        }
        if !domain.owner.is_valid() || domain.owner.role_definition(roles).is_none() {
            return Err(format!(
                "domain `{name}` references missing role incarnation {}",
                domain.owner.oid
            ));
        }
        crate::schema::domains::constraints::validate(&domain.definition, true)
            .map_err(|error| error.to_string())?;
        for identity in crate::schema::domains::constraints::identities(&domain.definition) {
            if !constraint_objects.insert(identity.object_id)
                || !constraint_oids.insert(identity.oid)
            {
                return Err(format!(
                    "duplicate domain constraint catalog identity for `{name}`"
                ));
            }
        }
    }
    Ok(())
}

/// Definition lookup for domain inheritance and constraint binding.
pub trait DomainCatalog {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain>;
}

pub fn domain_default_expression(
    catalog: &dyn DomainCatalog,
    ty: &crate::ColumnType,
) -> Option<crate::ast::Expr> {
    let crate::ColumnType::Domain { oid, base, .. } = ty else {
        return None;
    };
    catalog
        .domain_by_oid(*oid)
        .and_then(|domain| domain.definition.default)
        .or_else(|| domain_default_expression(catalog, base))
}

#[cfg(test)]
mod tests;
