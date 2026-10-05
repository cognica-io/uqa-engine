//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! State operations used by native role lifecycle execution.

use std::{collections::BTreeMap, ops::DerefMut};
use uqa_sql::catalog::roles::identity::RoleBinding;
use uqa_sql::{
    catalog::roles::{
        definition::RoleValidationContext, RoleDefinition, RoleIdentity, RoleMembership,
        RoleMembershipKey,
    },
    SQLError,
};

pub type RoleDefinitionWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, RoleDefinition>> + 'a>;
pub type RoleMembershipWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RoleMembershipKey, RoleMembership>> + 'a>;
pub trait RoleRegistry {
    fn write_roles(&self) -> RoleDefinitionWrite<'_>;
    fn write_memberships(&self) -> RoleMembershipWrite<'_>;
}
pub trait RolePublication {
    fn prepare_writer(&self) -> Result<(), SQLError>;
    fn persist_roles(
        &self,
        before: &BTreeMap<String, RoleDefinition>,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), SQLError>;
    fn persist_memberships(
        &self,
        before: &BTreeMap<RoleMembershipKey, RoleMembership>,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError>;
    fn catalog_changed(&self);
    fn set_current_role(&self, target: Option<RoleBinding>);
    fn set_session_authorization(&self, target: RoleBinding);
}
/// What depends on a role, as `checkSharedDependencies` reports it from `pg_shdepend`.
pub trait RoleSharedDependencies {
    /// The detail listing the objects that depend on `role`; `None` when nothing does. Projection may restore catalog state, so callers release registry guards before invoking it.
    fn role_dependency_detail(&self, role: RoleIdentity) -> Result<Option<String>, SQLError>;
}
#[derive(Clone, Copy)]
pub struct RoleExecutionContext<'a> {
    pub analysis: RoleValidationContext<'a>,
    pub registry: &'a dyn RoleRegistry,
    pub publication: &'a dyn RolePublication,
    pub dependencies: &'a dyn RoleSharedDependencies,
    pub locks: &'a dyn crate::row_locks::shared_objects::SharedObjectLockSession,
    pub temporary_roles: &'a dyn super::super::roles::temporary::TemporaryRoleDependencyReads,
}
