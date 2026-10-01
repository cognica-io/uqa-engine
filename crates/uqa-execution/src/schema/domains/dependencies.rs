//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove domains, enums, composite types and domain constraints, each once what depends on it has been removed.

use crate::schema::namespaces::NamespaceCatalogChanges;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{catalog::domain::StoredDomain, SQLError};

pub trait DomainDependencyCatalog {
    fn domain_definitions(&self) -> BTreeMap<String, StoredDomain>;
}
pub use crate::catalog::domain::DomainRegistryPublication;
pub struct DomainDependencyContext<'a> {
    pub catalog: &'a dyn DomainDependencyCatalog,
    pub publication: &'a dyn DomainRegistryPublication,
    pub enums: &'a dyn crate::catalog::enum_type::EnumRegistryPublication,
    pub composites: &'a dyn crate::catalog::composite_type::CompositeRegistryPublication,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

/// `RemoveConstraintById` for a domain's `CHECK` or `NOT NULL` constraint.
pub fn remove_domain_constraint(
    context: &DomainDependencyContext<'_>,
    domain: u32,
    name: &str,
) -> Result<(), SQLError> {
    let before = context.catalog.domain_definitions();
    let mut registry = before.clone();
    let stored = registry
        .values_mut()
        .find(|stored| stored.oid == domain)
        .ok_or_else(|| {
            SQLError::Internal(format!("domain {domain} disappeared before its constraint"))
        })?;
    stored
        .definition
        .checks
        .retain(|constraint| constraint.name.as_deref() != Some(name));
    if stored
        .definition
        .not_null
        .as_ref()
        .is_some_and(|constraint| constraint.name.as_deref() == Some(name))
    {
        stored.definition.not_null = None;
    }
    crate::catalog::domain::publish(context.publication, &before, registry)?;
    context.changes.catalog_registry_changed();
    Ok(())
}

/// `RemoveTypeById` for enums, domains and composite types, whose array types, and a composite type's relation, go with them. What depends on them has been removed.
pub fn remove_types(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<(), SQLError> {
    let before = context.catalog.domain_definitions();
    if before.values().any(|domain| targets.contains(&domain.oid)) {
        let mut registry = before.clone();
        registry.retain(|_, domain| !targets.contains(&domain.oid));
        crate::catalog::domain::publish(context.publication, &before, registry)?;
    }
    let enums_before = context.enums.enum_registry().clone();
    if enums_before
        .values()
        .any(|definition| targets.contains(&definition.oid))
    {
        let mut enums = enums_before.clone();
        enums.retain(|_, definition| !targets.contains(&definition.oid));
        crate::catalog::enum_type::publish(context.enums, &enums_before, enums)?;
    }
    let composites_before = context.composites.composite_registry().clone();
    if composites_before
        .values()
        .any(|definition| targets.contains(&definition.oid))
    {
        let mut composites = composites_before.clone();
        composites.retain(|_, definition| !targets.contains(&definition.oid));
        crate::catalog::composite_type::publish(
            context.composites,
            &composites_before,
            composites,
        )?;
    }
    context.changes.catalog_registry_changed();
    Ok(())
}
