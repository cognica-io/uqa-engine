//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! State operations used by native role lifecycle execution.

use std::{collections::BTreeMap, ops::DerefMut};
use uqa_sql::{
    catalog::roles::{
        definition::RoleValidationContext, dependencies::context::RoleDependencyCatalog,
        RoleDefinition, RoleMembership, RoleMembershipKey,
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
    fn persist_roles(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), SQLError>;
    fn persist_memberships(
        &self,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError>;
    fn catalog_changed(&self);
    fn set_current_role(&self, target: String);
}
#[derive(Clone, Copy)]
pub struct RoleExecutionContext<'a> {
    pub analysis: RoleValidationContext<'a>,
    pub registry: &'a dyn RoleRegistry,
    pub publication: &'a dyn RolePublication,
    pub dependencies: &'a dyn RoleDependencyCatalog,
}
