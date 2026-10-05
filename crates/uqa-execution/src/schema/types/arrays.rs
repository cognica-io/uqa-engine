//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated array types of enums, domains and composite types. A type that takes the name of a generated array moves that array out of the way, as `PostgreSQL`'s `moveArrayTypeName` does; any other holder of the name is a conflict.

use crate::catalog::{
    composite_type::{self, CompositeRegistryPublication},
    domain::{self, DomainRegistryPublication},
    enum_type::{self, EnumRegistryPublication},
};
use crate::schema::namespaces::relations::RelationCreationContext;
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::array_type_names::choose_array_type_name, SQLError};

/// The registries whose generated array types share the type namespace.
#[derive(Clone, Copy)]
pub struct UserTypeRegistries<'a> {
    pub enums: &'a dyn EnumRegistryPublication,
    pub domains: &'a dyn DomainRegistryPublication,
    pub composites: &'a dyn CompositeRegistryPublication,
}

/// Choose and reserve the array name of a type named `name` in `schema`.
pub fn reserve_array_name(
    creation: &RelationCreationContext<'_>,
    schema: &str,
    name: &str,
) -> Result<String, SQLError> {
    let array_name = choose_array_type_name(name, |candidate| {
        creation.type_name_in_use(&RelationIdentity::new(schema, candidate))
    });
    creation.reserve_type_name(&RelationIdentity::new(schema, &array_name).qualified_name())?;
    Ok(array_name)
}

/// Move a generated array named `requested` to a new generated name. Returns whether an array held the name.
pub fn displace_array_type(
    creation: &RelationCreationContext<'_>,
    registries: UserTypeRegistries<'_>,
    requested: &RelationIdentity,
) -> Result<bool, SQLError> {
    let enum_holder = registries
        .enums
        .enum_registry()
        .iter()
        .find(|(_, definition)| {
            definition.identity.schema == requested.schema
                && definition.array_name == requested.name
        })
        .map(|(key, _)| key.clone());
    if let Some(holder) = enum_holder {
        let moved = reserve_array_name(creation, &requested.schema, &requested.name)?;
        let before = registries.enums.enum_registry().clone();
        let mut registry = before.clone();
        registry
            .get_mut(&holder)
            .ok_or_else(|| SQLError::Internal("displaced enum array holder disappeared".into()))?
            .array_name = moved;
        enum_type::publish(registries.enums, &before, registry)?;
        return Ok(true);
    }
    let domain_holder = registries
        .domains
        .domain_registry()
        .iter()
        .find(|(_, domain)| {
            domain.identity.schema == requested.schema && domain.array_type_name() == requested.name
        })
        .map(|(key, _)| key.clone());
    if let Some(holder) = domain_holder {
        let moved = reserve_array_name(creation, &requested.schema, &requested.name)?;
        let before = registries.domains.domain_registry().clone();
        let mut registry = before.clone();
        registry
            .get_mut(&holder)
            .ok_or_else(|| SQLError::Internal("displaced domain array holder disappeared".into()))?
            .array_name = Some(moved);
        domain::publish(registries.domains, &before, registry)?;
        return Ok(true);
    }
    let composite_holder = registries
        .composites
        .composite_registry()
        .iter()
        .find(|(_, definition)| {
            definition.identity.schema == requested.schema
                && definition.array_name == requested.name
        })
        .map(|(key, _)| key.clone());
    if let Some(holder) = composite_holder {
        let moved = reserve_array_name(creation, &requested.schema, &requested.name)?;
        let before = registries.composites.composite_registry().clone();
        let mut registry = before.clone();
        registry
            .get_mut(&holder)
            .ok_or_else(|| {
                SQLError::Internal("displaced composite array holder disappeared".into())
            })?
            .array_name = moved;
        composite_type::publish(registries.composites, &before, registry)?;
        return Ok(true);
    }
    creation.runtime.displace_relation_array(requested)
}
