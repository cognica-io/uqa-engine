//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership commands retain pre-wait authority and coordinate target and grantor identities.

use super::{context::RoleExecutionContext, identity};
use crate::{
    catalog::security::roles::{locking::ROLE_CATALOG_CLASS_ID, persistence::RoleCatalogValues},
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::{AlterRoleStmt, CreateRoleStmt, GrantRoleStmt, RoleMembershipOptions, RoleSpecification},
    catalog::roles::{
        definition,
        identity::{RoleBinding, RoleSubject},
        memberships::command::{
            creator_membership, MembershipChange, MembershipInsertion, MembershipRecipients,
            MembershipRevocation, MembershipTarget,
        },
        resolve_role_specification, RoleDefinition, RoleReference,
    },
    SQLError,
};

mod overlay;
use overlay::MembershipOverlay;

struct MembershipWork<'a, 'context> {
    context: &'a RoleExecutionContext<'context>,
    current: RoleReference,
    created: Option<RoleDefinition>,
    memberships: MembershipOverlay,
}

impl MembershipWork<'_, '_> {
    fn roles(&self) -> Result<BTreeMap<String, RoleDefinition>, SQLError> {
        let roles = self.context.analysis.roles.role_definitions();
        self.with_created(&roles)
    }

    fn with_created(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<BTreeMap<String, RoleDefinition>, SQLError> {
        if let Some(created) = &self.created {
            definition::create_role_candidate(roles, &self.current, created.clone())
                .map(|(roles, _)| roles)
        } else {
            Ok(roles.clone())
        }
    }

    fn view(&self) -> Result<RoleCatalogValues, SQLError> {
        let roles = self.context.analysis.roles.role_definitions();
        let memberships = self.context.analysis.roles.role_memberships();
        Ok(RoleCatalogValues {
            roles: self.with_created(&roles)?,
            memberships: self.memberships.apply(&memberships)?,
        })
    }

    fn lock_target(&self, role: &RoleBinding) -> Result<(), SQLError> {
        let guard = self.context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: ROLE_CATALOG_CLASS_ID,
                oid: role.oid,
            },
            RelationLockMode::ShareUpdateExclusive,
        )?;
        self.context.locks.refresh_shared_catalog()?;
        // PostgreSQL keeps the captured target even if DROP completed while this lock waited.
        guard.retain();
        Ok(())
    }

    fn insert(&mut self, insertion: MembershipInsertion) -> Result<(), SQLError> {
        let oid = identity::reserve_membership_oid(
            self.context,
            &self.memberships.oids(),
            identity::allocate_oid,
        )?;
        if insertion.grantor.oid != 10 {
            let guard = self.context.locks.acquire_shared_catalog(
                SharedCatalogLock::Object {
                    class_id: ROLE_CATALOG_CLASS_ID,
                    oid: insertion.grantor.oid,
                },
                RelationLockMode::AccessShare,
            )?;
            self.context.locks.refresh_shared_catalog()?;
            if insertion.grantor.role_definition(&self.roles()?).is_none() {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role {} was concurrently dropped", insertion.grantor.oid),
                });
            }
            guard.retain();
        }
        let membership = insertion.with_oid(oid)?;
        if self.view()?.memberships.contains_key(&membership.key()) {
            return Err(SQLError::Routine {
                sqlstate: "23505".into(),
                message: "duplicate role membership".into(),
            });
        }
        self.memberships.insert(membership);
        Ok(())
    }

    fn command(
        &mut self,
        targets: Vec<RoleReference>,
        statement: &GrantRoleStmt,
    ) -> Result<(), SQLError> {
        let recipients =
            MembershipRecipients::bind(self.context.analysis.names, &self.roles()?, statement)?;
        for target in targets {
            let bound = {
                let RoleCatalogValues { roles, memberships } = self.view()?;
                MembershipTarget::authorize(
                    &roles,
                    &memberships,
                    &self.current,
                    &target,
                    &recipients,
                    statement,
                )?
            };
            self.target(bound)?;
        }
        Ok(())
    }

    fn target(&mut self, bound: MembershipTarget) -> Result<(), SQLError> {
        self.lock_target(&bound.role)?;
        if !bound.is_grant {
            let RoleCatalogValues { roles, memberships } = self.view()?;
            let mut plan = MembershipRevocation::new(&bound, &memberships);
            for member in &bound.members {
                if let Some(message) = plan.member(&roles, member)? {
                    self.context.analysis.notices.notice("WARNING", &message);
                }
            }
            for update in plan.into_updates() {
                self.memberships.update(update);
            }
            return Ok(());
        }
        bound.validate_graph(&self.view()?.memberships)?;
        for member in &bound.members {
            let change = {
                let RoleCatalogValues { roles, memberships } = self.view()?;
                bound.change_for_member(&roles, &memberships, member)?
            };
            match change {
                MembershipChange::Insert(insertion) => self.insert(insertion)?,
                MembershipChange::Update(updates) => {
                    for update in updates {
                        self.memberships.update(update);
                    }
                }
                MembershipChange::Notice { level, message } => {
                    self.context.analysis.notices.notice(level, &message);
                }
            }
        }
        Ok(())
    }

    fn publish(self) -> Result<(), SQLError> {
        if self.created.is_none() && self.memberships.is_empty() {
            return Ok(());
        }
        self.context.publication.prepare_writer()?;
        let mut roles = self.context.registry.write_roles();
        let mut memberships = self.context.registry.write_memberships();
        let next_roles = self.with_created(&roles)?;
        let next_memberships = self.memberships.apply(&memberships)?;
        if self.created.is_some() {
            self.context
                .publication
                .persist_roles(&roles, &next_roles)?;
        }
        if **memberships != next_memberships {
            self.context
                .publication
                .persist_memberships(&memberships, &next_memberships)?;
        }
        if self.created.is_some() {
            **roles = next_roles;
        }
        if **memberships != next_memberships {
            **memberships = next_memberships;
        }
        drop(memberships);
        drop(roles);
        self.context.publication.catalog_changed();
        Ok(())
    }
}

pub(super) fn alter_group(
    context: &RoleExecutionContext<'_>,
    statement: &AlterRoleStmt,
) -> Result<(), SQLError> {
    let mut work = MembershipWork {
        context,
        current: context.analysis.names.current_role(),
        created: None,
        memberships: MembershipOverlay::default(),
    };
    let RoleCatalogValues { roles, memberships } = work.view()?;
    let bound = MembershipTarget::authorize_group(
        context.analysis.names,
        &roles,
        &memberships,
        &work.current,
        statement,
    )?;
    work.target(bound)?;
    work.publish()
}

pub(super) fn grant(
    context: &RoleExecutionContext<'_>,
    statement: &GrantRoleStmt,
    targets: Vec<RoleReference>,
) -> Result<(), SQLError> {
    let mut work = MembershipWork {
        context,
        current: context.analysis.names.current_role(),
        created: None,
        memberships: MembershipOverlay::default(),
    };
    work.command(targets, statement)?;
    work.publish()
}

pub(super) fn create(
    context: &RoleExecutionContext<'_>,
    current: RoleReference,
    statement: &CreateRoleStmt,
    created: RoleDefinition,
    superuser: bool,
) -> Result<(), SQLError> {
    let mut work = MembershipWork {
        context,
        current,
        created: Some(created.clone()),
        memberships: MembershipOverlay::default(),
    };
    let automatic = if superuser {
        None
    } else {
        Some(creator_membership(&work.roles()?, &work.current, &created)?)
    };
    let base = GrantRoleStmt {
        granted_roles: Vec::new(),
        grantee_roles: vec![RoleSpecification::Named(created.name.clone())],
        is_grant: true,
        options: RoleMembershipOptions::default(),
        grantor: None,
        cascade: false,
    };
    if !statement.in_roles.is_empty() {
        let targets = statement
            .in_roles
            .iter()
            .map(|role| resolve_role_specification(context.analysis.names, role))
            .collect();
        work.command(targets, &base)?;
    }
    if let Some(automatic) = automatic {
        work.target(MembershipTarget {
            role: automatic.role,
            grantor: automatic.grantor,
            members: vec![automatic.member],
            is_grant: true,
            options: RoleMembershipOptions {
                admin: Some(true),
                inherit: Some(false),
                set: Some(false),
            },
            cascade: false,
        })?;
    }
    for (members, admin) in [
        (&statement.role_members, None),
        (&statement.admin_members, Some(true)),
    ] {
        if members.is_empty() {
            continue;
        }
        let command = GrantRoleStmt {
            grantee_roles: members.clone(),
            options: RoleMembershipOptions {
                admin,
                ..RoleMembershipOptions::default()
            },
            ..base.clone()
        };
        work.command(
            vec![RoleReference::Bound(std::sync::Arc::new(
                RoleBinding::from_definition(&created)?,
            ))],
            &command,
        )?;
    }
    work.publish()
}
