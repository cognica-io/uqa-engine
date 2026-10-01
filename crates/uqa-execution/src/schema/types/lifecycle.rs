//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ALTER TYPE | DOMAIN ... RENAME TO | SET SCHEMA | OWNER TO` with `PostgreSQL`'s check order for each command (`RenameType`, `AlterTypeNamespace`, `AlterTypeOwner`). The generated array type follows its element.

use super::{arrays, TypeLifecycleContext};
use crate::catalog::security::roles::locking::RoleLockContext;
use crate::catalog::{domain, enum_type, security::roles::dependencies::prepare_role_owner};
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterTypeObject, AlterTypeObjectAction, RoleSpecification, TypeObjectKind},
    catalog::roles::{require_set_role, resolve_role_specification, role_inherits},
    catalog::security::{object_acl, ownership::OwnerChangeAuthority},
    schema::{
        constraint_metadata::CatalogOidClass,
        type_objects::{resolve_type_object, ResolvedTypeObject, TypeObject},
    },
    SQLError,
};

pub fn alter_type_object(
    context: &TypeLifecycleContext<'_>,
    statement: AlterTypeObject,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    match statement.action {
        AlterTypeObjectAction::RenameTo(new_name) => {
            rename(context, statement.kind, &statement.name, &new_name)
        }
        AlterTypeObjectAction::SetSchema(schema) => {
            set_schema(context, statement.kind, &statement.name, &schema)
        }
        AlterTypeObjectAction::OwnerTo(owner) => {
            set_owner(context, statement.kind, &statement.name, &owner)
        }
    }
}

/// Serialize changes to one type until the owning transaction ends, then resolve the name again against the refreshed catalog.
pub(super) fn lock_and_resolve(
    context: &TypeLifecycleContext<'_>,
    name: &str,
) -> Result<ResolvedTypeObject, SQLError> {
    loop {
        let initial = resolve_type_object(&context.binding, name)?;
        let guard = context.identities.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: CatalogOidClass::Type.class_id(),
                oid: initial.oid(),
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.identities.locks.refresh_shared_catalog()?;
        let current = resolve_type_object(&context.binding, name)?;
        if current.oid() == initial.oid() {
            guard.retain();
            return Ok(current);
        }
    }
}

fn rename(
    context: &TypeLifecycleContext<'_>,
    kind: TypeObjectKind,
    name: &str,
    new_name: &str,
) -> Result<(), SQLError> {
    let resolved = lock_and_resolve(context, name)?;
    resolved.require_owner(&context.binding)?;
    resolved.require_domain_keyword(&context.binding, kind)?;
    resolved.reject_row_type(&context.binding)?;
    resolved.reject_array(&context.binding)?;
    let object = resolved.into_type_object(&context.binding)?;
    let schema = object.identity().schema.clone();
    let target = RelationIdentity::new(&schema, new_name);
    // RenameRelationInternal: a composite type renames its relation first, whose name no other relation may hold.
    if matches!(object, TypeObject::Composite(_)) {
        if context.creation.relation_name_in_use(&target) {
            return Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{new_name}\" already exists"),
            });
        }
        context.creation.reserve_name(&target.qualified_name())?;
    }
    // RenameTypeInternal: a generated array holding the new name moves aside; any other type, including this one, is a conflict.
    if context.creation.type_name_in_use(&target)
        && !arrays::displace_array_type(&context.creation, context.registries, &target)?
    {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("type \"{new_name}\" already exists"),
        });
    }
    context
        .creation
        .reserve_type_name(&target.qualified_name())?;
    // Displacement may have renamed this type's own array; its current name stays taken while the next one is chosen, as in RenameTypeInternal.
    let object = current_definition(context, object.oid())?;
    let array_name = arrays::reserve_array_name(&context.creation, &schema, new_name)?;
    publish_identity(context, object, target, Some(array_name))
}

fn set_schema(
    context: &TypeLifecycleContext<'_>,
    kind: TypeObjectKind,
    name: &str,
    schema: &str,
) -> Result<(), SQLError> {
    let resolved = lock_and_resolve(context, name)?;
    resolved.require_domain_keyword(&context.binding, kind)?;
    let temporary = context.creation.state.temporary_schema_name();
    // LookupCreationNamespace: the destination must exist and allow CREATE before ownership is checked.
    let destination = if schema == "pg_temp" {
        temporary.clone()
    } else {
        context.creation.creation_namespace(schema)?
    };
    resolved.require_owner(&context.binding)?;
    resolved.reject_array(&context.binding)?;
    // A table's row type fails after the destination's duplicate-name check, as AlterTypeNamespaceInternal orders them.
    if let Some(row_type) = resolved.row_type_identity() {
        if row_type.schema != destination
            && context
                .creation
                .type_name_in_use(&RelationIdentity::new(&destination, &row_type.name))
        {
            return Err(duplicate_in_schema(&row_type.name, &destination));
        }
        resolved.reject_row_type(&context.binding)?;
    }
    let object = resolved.into_type_object(&context.binding)?;
    let identity = object.identity().clone();
    if identity.schema == destination {
        return Ok(());
    }
    if destination == temporary || identity.schema == temporary {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot move objects into or out of temporary schemas".into(),
        });
    }
    let target = RelationIdentity::new(&destination, &identity.name);
    let array_target = RelationIdentity::new(&destination, object.array_name());
    // AlterTypeNamespaceInternal checks the type's name, then moves a composite type's relation, whose name no relation in the destination may hold, and then the array type.
    if context.creation.type_name_in_use(&target) {
        return Err(duplicate_in_schema(&target.name, &destination));
    }
    context
        .creation
        .reserve_type_name(&target.qualified_name())?;
    if matches!(object, TypeObject::Composite(_)) {
        if context.creation.relation_name_in_use(&target) {
            return Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!(
                    "relation \"{}\" already exists in schema \"{destination}\"",
                    target.name
                ),
            });
        }
        context.creation.reserve_name(&target.qualified_name())?;
    }
    if context.creation.type_name_in_use(&array_target) {
        return Err(duplicate_in_schema(&array_target.name, &destination));
    }
    context
        .creation
        .reserve_type_name(&array_target.qualified_name())?;
    publish_identity(context, object, target, None)
}

fn duplicate_in_schema(name: &str, schema: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("type \"{name}\" already exists in schema \"{schema}\""),
    }
}

fn set_owner(
    context: &TypeLifecycleContext<'_>,
    kind: TypeObjectKind,
    name: &str,
    requested: &RoleSpecification,
) -> Result<(), SQLError> {
    let locks = RoleLockContext {
        roles: context.creation.roles,
        session: context.identities.locks,
    };
    // The new owner is looked up before the type, as ExecAlterOwnerStmt does.
    let owner = locks.bind(&resolve_role_specification(
        context.creation.names,
        requested,
    ))?;
    let resolved = lock_and_resolve(context, name)?;
    resolved.require_domain_keyword(&context.binding, kind)?;
    resolved.reject_row_type(&context.binding)?;
    resolved.reject_array(&context.binding)?;
    let object = resolved.into_type_object(&context.binding)?;
    let current_user = context.creation.names.current_role();
    let candidate = prepare_role_owner(
        locks,
        &owner,
        || context.writer.prepare_writer(),
        |roles, memberships, new_owner| {
            let object = current_definition(context, object.oid())?;
            let previous = object.owner();
            if previous == owner.identity() {
                return Ok(None);
            }
            let superuser =
                uqa_sql::catalog::roles::memberships::role_is_superuser(roles, &current_user);
            if !superuser {
                if !role_inherits(roles, memberships, &current_user, &previous) {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!(
                            "must be owner of type {}",
                            format_type(context, object.oid())?
                        ),
                    });
                }
                require_set_role(roles, memberships, &current_user, new_owner)?;
                OwnerChangeAuthority {
                    roles,
                    memberships,
                    current_user: &current_user,
                    new_owner,
                }
                .require_schema_create(context.owner_schemas, &object.identity().schema)?;
            }
            Ok(Some((object, previous)))
        },
    )?;
    let Some((object, previous)) = candidate.value else {
        return Ok(());
    };
    let new_owner = owner.identity();
    match object {
        TypeObject::Enum(mut definition) => {
            definition.owner = new_owner;
            object_acl::rewrite_owner(&mut definition.usage_acl, previous, new_owner);
            let before = context.registries.enums.enum_registry().clone();
            let mut registry = before.clone();
            registry.insert(definition.identity.qualified_name(), definition);
            enum_type::publish(context.registries.enums, &before, registry)?;
        }
        TypeObject::Domain(mut definition) => {
            definition.owner = new_owner;
            object_acl::rewrite_owner(&mut definition.usage_acl, previous, new_owner);
            let before = context.registries.domains.domain_registry().clone();
            let mut registry = before.clone();
            registry.insert(definition.identity.qualified_name(), *definition);
            domain::publish(context.registries.domains, &before, registry)?;
        }
        // `ATExecChangeOwner` changes the composite relation's owner with the type's.
        TypeObject::Composite(mut definition) => {
            definition.owner = new_owner;
            object_acl::rewrite_owner(&mut definition.usage_acl, previous, new_owner);
            let before = context.registries.composites.composite_registry().clone();
            let mut registry = before.clone();
            registry.insert(definition.identity.qualified_name(), *definition);
            crate::catalog::composite_type::publish(
                context.registries.composites,
                &before,
                registry,
            )?;
        }
    }
    drop(candidate.memberships);
    drop(candidate.roles);
    context.changes.catalog_registry_changed();
    Ok(())
}

/// The committed or privately changed definition of the type with this OID.
pub(super) fn current_definition(
    context: &TypeLifecycleContext<'_>,
    oid: u32,
) -> Result<TypeObject, SQLError> {
    if let Some(definition) = context
        .registries
        .enums
        .enum_registry()
        .values()
        .find(|definition| definition.oid == oid)
    {
        return Ok(TypeObject::Enum(definition.clone()));
    }
    if let Some(definition) = context
        .registries
        .composites
        .composite_registry()
        .values()
        .find(|definition| definition.oid == oid)
    {
        return Ok(TypeObject::Composite(Box::new(definition.clone())));
    }
    context
        .registries
        .domains
        .domain_registry()
        .values()
        .find(|domain| domain.oid == oid)
        .map(|domain| TypeObject::Domain(Box::new(domain.clone())))
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {oid} does not exist"),
        })
}

/// Publish a type under a new schema or name, with its array renamed when `array_name` is given, and give dependent definitions the new name.
fn publish_identity(
    context: &TypeLifecycleContext<'_>,
    object: TypeObject,
    identity: RelationIdentity,
    array_name: Option<String>,
) -> Result<(), SQLError> {
    let oid = object.oid();
    match object {
        TypeObject::Enum(mut definition) => {
            let before = context.registries.enums.enum_registry().clone();
            let mut registry = before.clone();
            registry.remove(&definition.identity.qualified_name());
            definition.identity = identity.clone();
            if let Some(array_name) = array_name {
                definition.array_name = array_name;
            }
            registry.insert(identity.qualified_name(), definition);
            enum_type::publish(context.registries.enums, &before, registry)?;
        }
        TypeObject::Domain(mut definition) => {
            let before = context.registries.domains.domain_registry().clone();
            let mut registry = before.clone();
            registry.remove(&definition.identity.qualified_name());
            let array_name = array_name.unwrap_or_else(|| definition.array_type_name());
            definition.identity = identity.clone();
            definition.definition.name = identity.qualified_name();
            definition.array_name = Some(array_name);
            registry.insert(identity.qualified_name(), *definition);
            domain::publish(context.registries.domains, &before, registry)?;
        }
        TypeObject::Composite(mut definition) => {
            let before = context.registries.composites.composite_registry().clone();
            let mut registry = before.clone();
            registry.remove(&definition.identity.qualified_name());
            definition.identity = identity.clone();
            if let Some(array_name) = array_name {
                definition.array_name = array_name;
            }
            registry.insert(identity.qualified_name(), *definition);
            crate::catalog::composite_type::publish(
                context.registries.composites,
                &before,
                registry,
            )?;
        }
    }
    rewrite_domain_bases(context, oid, &identity)?;
    rewrite_composite_attributes(context, oid, &identity)?;
    context.dependents.rewrite_type_references(oid, &identity)?;
    context.changes.catalog_registry_changed();
    Ok(())
}

/// Domains carry their base type's catalog name.
fn rewrite_domain_bases(
    context: &TypeLifecycleContext<'_>,
    oid: u32,
    identity: &RelationIdentity,
) -> Result<(), SQLError> {
    let before = context.registries.domains.domain_registry().clone();
    let mut registry = before.clone();
    let mut changed = false;
    for domain in registry.values_mut() {
        changed |= domain.definition.base.rename_user_type(oid, identity);
    }
    if changed {
        domain::publish(context.registries.domains, &before, registry)?;
    }
    Ok(())
}

/// Composite attributes carry their types' catalog names.
fn rewrite_composite_attributes(
    context: &TypeLifecycleContext<'_>,
    oid: u32,
    identity: &RelationIdentity,
) -> Result<(), SQLError> {
    let before = context.registries.composites.composite_registry().clone();
    let mut registry = before.clone();
    let mut changed = false;
    for definition in registry.values_mut() {
        for attribute in &mut definition.attributes {
            changed |= attribute.ty.rename_user_type(oid, identity);
        }
    }
    if changed {
        crate::catalog::composite_type::publish(context.registries.composites, &before, registry)?;
    }
    Ok(())
}

fn format_type(context: &TypeLifecycleContext<'_>, oid: u32) -> Result<String, SQLError> {
    context
        .binding
        .catalog
        .format_drop_type(i64::from(oid))
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::Internal(format!("type {oid} disappeared")))
}
