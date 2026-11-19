//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence owners, grantees and grantors retain their original role incarnations.

use super::{role_bindings, SequenceSecurity};
use crate::catalog::roles::RoleDefinitions;
use crate::catalog::roles::{identity::RoleBinding, RoleIdentity, RoleReference};
mod resolution;

use uqa_core::{
    catalog_role::BoundAclEntry,
    catalog_sequence::{SequenceAclEntry, SequencePrivileges},
};

pub type BoundSequenceAclEntry = BoundAclEntry<SequencePrivileges>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSequenceSecurity {
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundSequenceAclEntry>>,
}

#[cfg(test)]
mod tests;

impl BoundSequenceSecurity {
    pub fn from_row(row: uqa_core::catalog_sequence::BoundSequenceSecurity) -> Self {
        Self {
            role_owner: row.role_owner,
            acl: row.acl,
        }
    }

    pub fn row(&self) -> uqa_core::catalog_sequence::BoundSequenceSecurity {
        uqa_core::catalog_sequence::BoundSequenceSecurity {
            role_owner: self.role_owner,
            acl: self.acl.clone(),
        }
    }

    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            role_owner,
            acl: None,
        }
    }

    pub fn owner_reference(
        &self,
        roles: &RoleDefinitions,
    ) -> Result<RoleReference, crate::SQLError> {
        let name = role_bindings::role_name(roles, self.role_owner, "sequence")
            .map_err(crate::SQLError::Internal)?;
        Ok(RoleReference::Bound(std::sync::Arc::new(
            RoleBinding::from_definition(&roles[name])?,
        )))
    }

    pub fn bind(security: &SequenceSecurity, roles: &RoleDefinitions) -> Result<Self, String> {
        let bind = |name: &str| role_bindings::bind_role(roles, name, "sequence");
        let entries = |entries: &[SequenceAclEntry]| {
            entries
                .iter()
                .map(|entry| {
                    Ok(BoundSequenceAclEntry {
                        role: entry.role.role_name().map(bind).transpose()?,
                        grantor: bind(entry.grantor.as_deref().unwrap_or(&security.role_owner))?,
                        privileges: entry.privileges,
                        grant_options: entry.grant_options,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        };
        Ok(Self {
            role_owner: bind(&security.role_owner)?,
            acl: security.acl.as_deref().map(entries).transpose()?,
        })
    }

    /// Resolve display names only through the role view accompanying this authority snapshot.
    pub fn resolve(&self, roles: &RoleDefinitions) -> Result<SequenceSecurity, String> {
        self.resolve_with_control(roles, &uqa_core::memory::ProductionControl::uncontrolled())
            .map(|value| value.into_parts().0)
            .map_err(resolution::diagnostic)
    }

    pub fn validate(&self, roles: &RoleDefinitions) -> Result<(), String> {
        self.validate_with_control(roles, &uqa_core::memory::ProductionControl::uncontrolled())
            .map_err(resolution::diagnostic)
    }

    pub fn depends_on(&self, role: RoleIdentity) -> bool {
        self.role_owner == role
            || self
                .acl
                .iter()
                .flatten()
                .any(|entry| entry.role == Some(role) || entry.grantor == role)
    }
}
