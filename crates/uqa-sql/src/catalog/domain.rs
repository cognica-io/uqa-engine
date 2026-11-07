//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{ColumnType, CreateDomain};
use crate::catalog::roles::RoleDefinitions;
use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use super::roles::RoleReference;

mod production;
pub mod validation;

pub fn domain_object_oid(object_id: &[u8; 16]) -> u32 {
    u32::try_from(super::oids::stable_object_oid("domain", object_id))
        .expect("catalog OIDs fit in u32")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDomain<Owner = RoleIdentity> {
    pub object_id: [u8; 16],
    pub oid: u32,
    /// The OID of the generated array type, allocated before the domain's own as `DefineDomain` does. Domains created before array OIDs were recorded derive both OIDs from their identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_oid: Option<u32>,
    pub identity: RelationIdentity,
    pub owner: Owner,
    pub definition: CreateDomain,
    /// Name of the generated array type, chosen as `makeArrayTypeName` chooses it. Domains created before array names were recorded use the uncontested name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_name: Option<String>,
    /// Explicit `USAGE` privileges; `None` is the default ACL (PUBLIC and the owner).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_acl: Option<Vec<crate::ast::ObjectAclEntry>>,
}

impl<Owner> StoredDomain<Owner> {
    pub fn column_type(&self) -> ColumnType {
        ColumnType::Domain {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            array_oid: self.array_oid,
            collation_oid: self.definition.collation_oid,
            base: Box::new(self.definition.base.clone()),
        }
    }

    /// The name of the domain's generated array type.
    pub fn array_type_name(&self) -> String {
        self.array_name
            .clone()
            .unwrap_or_else(|| super::array_type_names::array_type_name(&self.identity.name, 0))
    }
}

impl StoredDomain<String> {
    /// Bind a legacy name only when the catalog restoration owner allows conversion.
    pub fn bind_owner(self, roles: &RoleDefinitions) -> Result<StoredDomain, String> {
        let owner = RoleReference::Named(self.owner)
            .bind(roles)
            .map_err(|error| error.to_string())?
            .identity();
        Ok(StoredDomain {
            object_id: self.object_id,
            oid: self.oid,
            array_oid: self.array_oid,
            identity: self.identity,
            owner,
            definition: self.definition,
            array_name: self.array_name,
            usage_acl: self.usage_acl,
        })
    }
}

/// Validate the complete catalog before any durable conversion or registry publication.
pub fn validate_domain_registry(
    registry: &impl super::type_definitions::TypeDefinitionView<StoredDomain>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validation::finish(registry, roles, validation::DomainValidationMode::Complete)
}

/// Early restoration validates authority and supplied identities before any missing legacy constraint metadata is allocated.
pub fn validate_domain_definitions(
    registry: &impl super::type_definitions::TypeDefinitionView<StoredDomain>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validation::finish(
        registry,
        roles,
        validation::DomainValidationMode::Definitions,
    )
}

/// Definition lookup for domain inheritance and constraint binding.
pub trait DomainCatalog {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain>;

    /// Native catalogs copy the selected declaration under the caller's control. Legacy external callbacks retain responsibility for producing their input.
    fn domain_by_oid_with_control(
        &self,
        oid: u32,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<StoredDomain>>, crate::SQLError> {
        control.check()?;
        self.domain_by_oid(oid)
            .map(|domain| {
                domain
                    .retain_external_with_control(control)
                    .map_err(Into::into)
            })
            .transpose()
    }
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
