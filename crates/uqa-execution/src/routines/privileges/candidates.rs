//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared command authority while constructing user and builtin routine ACL candidates.

use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{catalog_acl::AclGrantee, catalog_role::RoleIdentity};
use uqa_sql::{
    ast::{GrantRoutineStmt, RoutineRevokeBehavior},
    catalog::roles::{RoleDefinition, RoleMembership, RoleMembershipKey, RoleReference},
    routines::{lifecycle::RoutineRegistry, security as analysis},
    SQLError, SQLNotice,
};

pub(super) struct GrantCandidateContext<'a> {
    pub(super) statement: &'a GrantRoutineStmt,
    pub(super) current_user: &'a RoleReference,
    pub(super) roles: &'a BTreeMap<String, RoleDefinition>,
    pub(super) memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
    pub(super) grantees: &'a [AclGrantee],
    pub(super) bound_grantees: &'a [Option<RoleIdentity>],
    pub(super) notices: &'a mut Vec<SQLNotice>,
    pub(super) dependencies: &'a mut BTreeSet<String>,
}

impl GrantCandidateContext<'_> {
    pub(super) fn user(
        &mut self,
        name: &str,
        position: usize,
        next: &mut RoutineRegistry,
    ) -> Result<(), SQLError> {
        let existing = next[name][position].clone();
        let grantor = analysis::select_routine_acl_grantor(
            &existing.def,
            self.current_user,
            self.roles,
            self.memberships,
        )?;
        analysis::grants::validate_target_options(self.statement, self.grantees)?;
        let mut def = existing.def.clone();
        if let Some(grantor) = grantor {
            if self.statement.is_grant {
                for grantee in self.bound_grantees {
                    analysis::grant_routine_acl(
                        &mut def,
                        *grantee,
                        grantor,
                        self.statement.grant_option,
                    )?;
                }
            } else {
                for grantee in self.bound_grantees {
                    analysis::revoke_routine_acl(
                        &mut def,
                        *grantee,
                        grantor,
                        self.statement.grant_option_only,
                        self.statement.revoke_behavior == RoutineRevokeBehavior::Cascade,
                    )?;
                }
            }
        } else {
            self.notices.push(analysis::routine_acl_warning(
                self.statement.is_grant,
                &existing.def.name,
            ));
        }
        // The command stores the ACL even when it grants or revokes nothing.
        analysis::make_routine_acl_explicit(&mut def)?;
        analysis::binding::added_routine_acl_roles(
            &existing.def,
            &def,
            self.roles,
            self.dependencies,
        )?;
        // `ExecGrant_common` stores a new catalog tuple even when its ACL is unchanged.
        next.get_mut(name).expect("resolved routine key")[position] =
            super::super::catalog::revision::replacement(def, existing.body.clone())?;
        Ok(())
    }
}
