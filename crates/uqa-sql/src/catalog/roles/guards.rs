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
/// Roles and memberships are acquired separately so validation can retain its original error and lock order.
pub trait RoleCatalogGuards {
    fn role_definitions(&self) -> RoleDefinitionRead<'_>;
    fn role_memberships(&self) -> RoleMembershipRead<'_>;
}
