//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An ACL command binds its recipients once and projects those identities after catalog waits.

use crate::{
    ast::{AclRoleSpecification, RoleSpecification},
    catalog::roles::{
        identity::RoleBinding, resolve_acl_role_specification, resolve_role_specification,
        RoleDefinition, RoleReference, RoleReferenceNames,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::catalog_acl::AclGrantee;

#[derive(Clone, Default)]
pub struct AclCommandRoles {
    bound: Option<BoundAclRoles>,
}

#[derive(Clone)]
struct BoundAclRoles {
    grantees: Vec<Option<RoleBinding>>,
    grantor: Option<RoleBinding>,
    current_user: RoleReference,
}

pub struct ResolvedAclRoles {
    pub grantees: Vec<AclGrantee>,
    pub requested_grantor: Option<String>,
    pub current_user: RoleReference,
}

impl AclCommandRoles {
    /// Preserve each object's validation order before retaining the complete command's role arguments.
    pub fn resolve_validated(
        &mut self,
        names: &dyn RoleReferenceNames,
        roles: &BTreeMap<String, RoleDefinition>,
        grantees: &[AclRoleSpecification],
        grantor: Option<&RoleSpecification>,
        validate: impl FnOnce(&ResolvedAclRoles) -> Result<(), SQLError>,
    ) -> Result<ResolvedAclRoles, SQLError> {
        let resolved = if let Some(bound) = &self.bound {
            bound.resolve(roles)?
        } else {
            ResolvedAclRoles {
                grantees: grantees
                    .iter()
                    .map(|role| resolve_acl_role_specification(names, role, roles))
                    .collect::<Result<_, _>>()?,
                requested_grantor: grantor
                    .map(|role| resolve_role_specification(names, role).catalog_name(roles))
                    .transpose()?,
                current_user: names.current_role(),
            }
        };
        validate(&resolved)?;
        if self.bound.is_none() {
            self.bound = Some(BoundAclRoles {
                grantees: resolved
                    .grantees
                    .iter()
                    .map(|grantee| {
                        grantee
                            .role_name()
                            .map(|name| RoleReference::from(name).bind(roles))
                            .transpose()
                    })
                    .collect::<Result<_, _>>()?,
                grantor: resolved
                    .requested_grantor
                    .as_deref()
                    .map(|name| RoleReference::from(name).bind(roles))
                    .transpose()?,
                current_user: match &resolved.current_user {
                    RoleReference::Bound(_) => resolved.current_user.clone(),
                    RoleReference::Named(_) => RoleReference::Bound(std::sync::Arc::new(
                        resolved.current_user.bind(roles)?,
                    )),
                },
            });
        }
        Ok(resolved)
    }
}

impl BoundAclRoles {
    fn resolve(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<ResolvedAclRoles, SQLError> {
        let name = |binding: &RoleBinding| {
            binding.revalidate(roles)?;
            binding.require_name(roles).map(str::to_owned)
        };
        Ok(ResolvedAclRoles {
            grantees: self
                .grantees
                .iter()
                .map(|binding| {
                    binding.as_ref().map_or_else(
                        || Ok(AclGrantee::Public),
                        |binding| name(binding).map(AclGrantee::Role),
                    )
                })
                .collect::<Result<_, _>>()?,
            requested_grantor: self.grantor.as_ref().map(name).transpose()?,
            current_user: self.current_user.clone(),
        })
    }
}

#[cfg(test)]
mod tests;
