//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence privilege inquiry, target binding and access checks.

use super::{
    sequence::{self as acl, role_has_privilege, AclPrivilege, PrivilegeCheck},
    BoundSequenceSecurity,
};
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    catalog::{
        resolution::RelationResolution,
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
    },
    SQLError,
};
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::{RelationIdentity, Value};

pub type SequenceSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, BoundSequenceSecurity>> + 'a>;
pub trait SequenceSecurityCatalog {
    fn security_read(&self) -> SequenceSecurityRead<'_>;
}
pub trait SequencePrivilegeResolution {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError>;
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError>;
}
pub trait SequenceTablePrivilegeInquiry {
    fn sequence_table_privileges(
        &self,
        relation: &RelationIdentity,
        subject: &dyn RoleSubject,
        checks: &[super::table::TablePrivilegeCheck],
    ) -> Result<bool, SQLError>;
}

pub struct SequencePrivilegeInquiry<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub security: &'a dyn SequenceSecurityCatalog,
    pub resolution: &'a dyn SequencePrivilegeResolution,
}

impl SequencePrivilegeInquiry<'_> {
    pub fn ensure_sequence_nextval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Update],
        )
    }

    pub fn ensure_sequence_currval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Select],
        )
    }

    pub fn ensure_sequence_setval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(name, relation, &[AclPrivilege::Update])
    }

    fn ensure_sequence_value_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
        privileges: &[AclPrivilege],
    ) -> Result<(), SQLError> {
        let security = self
            .security
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        let current_user = self.names.current_role();
        let roles = self.roles.role_definitions();
        let security = security.resolve(&roles).map_err(SQLError::Internal)?;
        let memberships = self.roles.role_memberships();
        if privileges.iter().any(|privilege| {
            role_has_privilege(
                &security,
                &current_user,
                PrivilegeCheck {
                    privilege: *privilege,
                    grant_option: false,
                },
                &roles,
                &memberships,
            )
        }) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for sequence {}", relation.name),
        })
    }

    pub fn has_sequence_privilege_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        let Some(arguments) = SequencePrivilegeArguments::parse(arguments)? else {
            return Ok(Value::Null);
        };
        let request = arguments.bind(self.names, self.roles)?;
        let Some((_, relation)) = request.target.resolve(self.resolution)? else {
            return Ok(Value::Null);
        };
        request.evaluate(&relation, self.roles, self.security)
    }

    pub fn role_has_sequence_table_privilege(
        &self,
        relation: &RelationIdentity,
        subject: &(impl RoleSubject + ?Sized),
        privilege: super::table::TableAclPrivilege,
        grant_option: bool,
    ) -> Result<bool, SQLError> {
        self.role_has_sequence_table_privileges(
            relation,
            subject,
            &[super::table::TablePrivilegeCheck {
                privilege,
                grant_option,
            }],
        )
    }

    pub fn role_has_sequence_table_privileges(
        &self,
        relation: &RelationIdentity,
        subject: &(impl RoleSubject + ?Sized),
        checks: &[super::table::TablePrivilegeCheck],
    ) -> Result<bool, SQLError> {
        let mut checks = checks
            .iter()
            .filter_map(|check| {
                let privilege = match check.privilege {
                    super::table::TableAclPrivilege::Select => AclPrivilege::Select,
                    super::table::TableAclPrivilege::Update => AclPrivilege::Update,
                    super::table::TableAclPrivilege::Insert
                    | super::table::TableAclPrivilege::Delete
                    | super::table::TableAclPrivilege::Truncate
                    | super::table::TableAclPrivilege::References
                    | super::table::TableAclPrivilege::Trigger
                    | super::table::TableAclPrivilege::Maintain => return None,
                };
                Some(PrivilegeCheck {
                    privilege,
                    grant_option: check.grant_option,
                })
            })
            .peekable();
        if checks.peek().is_none() {
            return Ok(false);
        }
        let security = self
            .security
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let roles = self.roles.role_definitions();
        let security = security.resolve(&roles).map_err(SQLError::Internal)?;
        let memberships = self.roles.role_memberships();
        Ok(checks.any(|check| role_has_privilege(&security, subject, check, &roles, &memberships)))
    }

    pub fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError> {
        let security = self
            .security
            .security_read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        let roles = self.roles.role_definitions();
        let owner = security.owner_reference(&roles)?;
        crate::schema::sequences::ownership::require_sequence_ownership(&relation.name, {
            let current = self.names.current_role();
            let memberships = self.roles.role_memberships();
            crate::catalog::roles::role_inherits(&roles, &memberships, &current, &owner)
        })?;
        let owner = owner.catalog_name(&roles)?;
        Ok(owner)
    }
}

impl SequenceTablePrivilegeInquiry for SequencePrivilegeInquiry<'_> {
    fn sequence_table_privileges(
        &self,
        relation: &RelationIdentity,
        subject: &dyn RoleSubject,
        checks: &[super::table::TablePrivilegeCheck],
    ) -> Result<bool, SQLError> {
        self.role_has_sequence_table_privileges(relation, subject, checks)
    }
}

mod value;
pub use value::{
    missing_sequence, SequencePrivilegeArguments, SequencePrivilegeRequest, SequencePrivilegeTarget,
};

#[cfg(test)]
mod tests;
