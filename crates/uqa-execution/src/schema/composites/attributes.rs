//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute removal from a standalone composite type: the attribute keeps its number as a dropped attribute, and stored values of the type lose the field.

use uqa_core::RelationIdentity;
use uqa_sql::expr::composites::AttributeChange;
use uqa_sql::SQLError;

use super::values::{rewrite_composite_values, CompositeValueContext};
use crate::catalog::composite_type::{self, CompositeRegistryPublication};
use crate::schema::namespaces::NamespaceCatalogChanges;

pub struct CompositeAttributeContext<'a> {
    pub publication: &'a dyn CompositeRegistryPublication,
    pub values: CompositeValueContext<'a>,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

/// `RemoveAttributeById` for an attribute of the composite relation `relation`.
pub fn drop_composite_attribute(
    context: &CompositeAttributeContext<'_>,
    relation: &RelationIdentity,
    name: &str,
) -> Result<(), SQLError> {
    let key = relation.qualified_name();
    let before = context.publication.composite_registry().clone();
    let definition = before.get(&key).ok_or_else(|| {
        SQLError::Internal(format!(
            "composite type `{key}` disappeared before its attribute"
        ))
    })?;
    rewrite_composite_values(
        &context.values,
        definition.oid,
        &AttributeChange::Drop(name.to_string()),
    )?;
    let mut registry = before.clone();
    let attribute = registry
        .get_mut(&key)
        .and_then(|definition| {
            definition
                .attributes
                .iter_mut()
                .find(|attribute| !attribute.dropped && attribute.name == name)
        })
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "attribute `{name}` of composite type `{key}` disappeared before its removal"
            ))
        })?;
    attribute.dropped = true;
    composite_type::publish(context.publication, &before, registry)?;
    context.changes.catalog_registry_changed();
    Ok(())
}
