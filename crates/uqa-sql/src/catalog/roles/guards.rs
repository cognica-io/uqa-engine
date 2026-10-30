//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained read guards for role and membership catalogs during authorization and publication.
use super::{RoleDefinition, RoleMembership, RoleMembershipKey};
use std::{collections::BTreeMap, ops::Deref};
pub type RoleDefinitionRead<'a> = Box<dyn Deref<Target = BTreeMap<String, RoleDefinition>> + 'a>;
pub type RoleMembershipRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RoleMembershipKey, RoleMembership>> + 'a>;
/// Observe role definitions and memberships under the owning catalog's original lock order.
pub type RoleCatalogVisitor<'a> = dyn FnMut(
        &BTreeMap<String, RoleDefinition>,
        &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), crate::SQLError>
    + 'a;

/// Roles and memberships are acquired separately so validation can retain its original error and lock order.
pub trait RoleCatalogGuards {
    fn role_definitions(&self) -> RoleDefinitionRead<'_>;
    fn role_memberships(&self) -> RoleMembershipRead<'_>;

    /// Native catalogs lend stack-held guards; external catalogs retain their existing guard callbacks.
    fn with_role_catalogs_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut RoleCatalogVisitor<'_>,
    ) -> Result<(), crate::SQLError> {
        control.check()?;
        let roles = self.role_definitions();
        let memberships = self.role_memberships();
        read(&roles, &memberships)?;
        Ok(control.check()?)
    }

    /// Capture current definitions for explicit inquiry subjects without requiring publication into a retained catalog.
    fn inquiry_role_definitions(&self) -> Result<RoleDefinitionRead<'_>, crate::SQLError> {
        Ok(self.role_definitions())
    }
}
