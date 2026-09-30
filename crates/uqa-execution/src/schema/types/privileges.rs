//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `GRANT | REVOKE USAGE ON TYPE | DOMAIN` through retained role state, in `ExecuteGrantStmt` order: the grantor, the target types, the grantees and the privilege names, then each type's checks and ACL change.

use super::{
    lifecycle::{current_definition, lock_and_resolve},
    TypeLifecycleContext,
};
use crate::catalog::security::roles::{
    dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use crate::catalog::{domain, enum_type};
use std::collections::BTreeSet;
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{
    ast::{GrantTypeStmt, ObjectAclEntry, TypeRevokeBehavior},
    catalog::roles::{
        identity::RoleSubject, memberships::role_is_superuser, role_inherits, RoleDefinition,
    },
    catalog::security::{
        acl_command::{AclCommandRoles, ResolvedAclRoles},
        object_acl,
        type_privileges::requested_type_usage,
    },
    schema::type_objects::{ResolvedTypeObject, TypeObject},
    SQLError,
};

pub fn grant_type(
    context: &TypeLifecycleContext<'_>,
    statement: &GrantTypeStmt,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    // GRANTED BY is checked before the targets, which are locked before any role state is retained.
    if let Some(grantor) = statement.grantor.as_ref() {
        let roles = context.creation.roles.role_definitions();
        let requested =
            uqa_sql::catalog::roles::resolve_role_specification(context.creation.names, grantor)
                .catalog_name(&roles)?;
        validate_requested_grantor(&requested, &context.creation.names.current_role(), &roles)?;
    }
    let mut targets = Vec::with_capacity(statement.names.len());
    for name in &statement.names {
        targets.push(lock_and_resolve(context, name)?);
    }
    let mut command_roles = AclCommandRoles::default();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value: TypePrivilegeCandidate { changes, notices },
        ..
    } = prepare_role_dependencies(
        &RoleLockContext {
            roles: context.creation.roles,
            session: context.identities.locks,
        },
        || context.writer.prepare_writer(),
        || prepare(context, statement, &targets, &mut command_roles),
    )?;
    for object in changes {
        publish(context, object)?;
    }
    drop(memberships);
    drop(roles);
    for notice in notices {
        context.notices.notice(notice);
    }
    context.changes.catalog_registry_changed();
    Ok(())
}

struct TypePrivilegeCandidate {
    changes: Vec<TypeObject>,
    notices: Vec<uqa_sql::SQLNotice>,
}

fn prepare<'a>(
    context: &'a TypeLifecycleContext<'_>,
    statement: &GrantTypeStmt,
    targets: &[ResolvedTypeObject],
    command_roles: &mut AclCommandRoles,
) -> Result<RoleDependencyCandidate<'a, TypePrivilegeCandidate>, SQLError> {
    let roles = context.creation.roles.role_definitions();
    let ResolvedAclRoles {
        grantees,
        current_user,
        ..
    } = command_roles.resolve_validated(
        context.creation.names,
        &roles,
        &statement.grantees,
        statement.grantor.as_ref(),
        |resolved| {
            if let Some(grantor) = resolved.requested_grantor.as_deref() {
                validate_requested_grantor(grantor, &resolved.current_user, &roles)?;
            }
            validate_grantees(resolved, &roles)
        },
    )?;
    let usage = requested_type_usage(&statement.privileges, statement.kind)?;
    let grantees = object_acl::bind_grantees(&grantees, &roles)?;
    let memberships = context.creation.roles.role_memberships();
    let superuser = role_is_superuser(&roles, &current_user);
    let mut changes: Vec<TypeObject> = Vec::new();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    for target in targets {
        target.require_grant_target(&context.binding, statement.kind)?;
        if statement.is_grant && statement.grant_option && grantees.iter().any(Option::is_none) {
            return Err(SQLError::Routine {
                sqlstate: "0LP01".into(),
                message: "grant options can only be granted to roles".into(),
            });
        }
        // A type listed twice sees its earlier change.
        let object = match changes.iter().find(|object| object.oid() == target.oid()) {
            Some(object) => object.clone(),
            None => {
                target.clone().into_type_object(&context.binding)?;
                current_definition(context, target.oid())?
            }
        };
        let owner = object.owner();
        let before = object.usage_acl().map(<[ObjectAclEntry]>::to_vec);
        let local_name = object.identity().name.clone();
        let grantor = object_acl::select_grantor(
            owner,
            before.as_deref(),
            &current_user,
            &roles,
            &memberships,
        );
        let mut acl = before.clone();
        if let Some(grantor) = grantor.filter(|_| usage) {
            apply_privileges(statement, owner, &mut acl, &grantees, grantor)?;
        } else {
            let has_role =
                |role: &RoleIdentity| role_inherits(&roles, &memberships, &current_user, role);
            // restrict_and_check_grant: holding no privilege at all is an error, otherwise nothing is granted.
            if !object_acl::privilege_allowed(&owner, before.as_deref(), false, superuser, has_role)
            {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("permission denied for type {local_name}"),
                });
            }
            notices.push(object_acl::acl_warning(statement.is_grant, &local_name));
        }
        // The default privileges become explicit even when nothing is granted, and no role other than the owner and PUBLIC enters the list then.
        let acl = object_acl::explicit_acl(owner, acl);
        object_acl::added_acl_roles(
            (owner, before.as_deref()),
            (owner, acl.as_deref()),
            &roles,
            "type",
            &mut dependencies,
        )?;
        if acl != before {
            let changed = with_acl(object, acl);
            changes.retain(|object| object.oid() != changed.oid());
            changes.push(changed);
        }
    }
    Ok(RoleDependencyCandidate {
        value: TypePrivilegeCandidate { changes, notices },
        memberships,
        roles,
        dependencies,
    })
}

/// `GRANTED BY` names an existing role, and only the current user.
fn validate_requested_grantor(
    grantor: &str,
    current_user: &uqa_sql::catalog::roles::RoleReference,
    roles: &std::collections::BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    if !roles.contains_key(grantor) {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{grantor}\" does not exist"),
        });
    }
    if current_user.role_name(roles) != Some(grantor) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "grantor must be current user".into(),
        });
    }
    Ok(())
}

fn validate_grantees(
    resolved: &ResolvedAclRoles,
    roles: &std::collections::BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for grantee in &resolved.grantees {
        if let Some(name) = grantee
            .role_name()
            .filter(|name| !roles.contains_key(*name))
        {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{name}\" does not exist"),
            });
        }
    }
    Ok(())
}

/// Grant or revoke the command's privileges from `grantor` in `acl`. Returns whether anything was granted or revoked: a revocation that finds no entry of the grantor removes nothing.
fn apply_privileges(
    statement: &GrantTypeStmt,
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
    grantees: &[Option<RoleIdentity>],
    grantor: RoleIdentity,
) -> Result<(), SQLError> {
    for grantee in grantees {
        if statement.is_grant {
            object_acl::grant(owner, acl, *grantee, grantor, statement.grant_option);
        } else {
            object_acl::revoke(
                owner,
                acl,
                *grantee,
                grantor,
                statement.grant_option_only,
                statement.revoke_behavior == TypeRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok(())
}

fn with_acl(object: TypeObject, acl: Option<Vec<ObjectAclEntry>>) -> TypeObject {
    match object {
        TypeObject::Enum(mut definition) => {
            definition.usage_acl = acl;
            TypeObject::Enum(definition)
        }
        TypeObject::Domain(mut definition) => {
            definition.usage_acl = acl;
            TypeObject::Domain(definition)
        }
    }
}

fn publish(context: &TypeLifecycleContext<'_>, object: TypeObject) -> Result<(), SQLError> {
    match object {
        TypeObject::Enum(definition) => {
            let before = context.registries.enums.enum_registry().clone();
            let mut registry = before.clone();
            registry.insert(definition.identity.qualified_name(), definition);
            enum_type::publish(context.registries.enums, &before, registry)
        }
        TypeObject::Domain(definition) => {
            let before = context.registries.domains.domain_registry().clone();
            let mut registry = before.clone();
            registry.insert(definition.identity.qualified_name(), *definition);
            domain::publish(context.registries.domains, &before, registry)
        }
    }
}
