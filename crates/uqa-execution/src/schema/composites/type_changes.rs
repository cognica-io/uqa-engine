//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare against the original descriptor, then publish composite attribute types after removals and before additions.

use super::alteration::CompositeAlterationContext;
use crate::schema::deletion::catalog_dependencies;
use uqa_sql::ast::CompositeAttributeDefinition;
use uqa_sql::catalog::composite_type::{StoredComposite, StoredCompositeAttribute};
use uqa_sql::catalog::dependencies::ObjectAddress;
use uqa_sql::schema::composites::type_changes::{
    prepare_attribute_type, reject_field_dependents, FieldDependent,
};
use uqa_sql::SQLError;

mod dependents;

pub(super) fn prepare(
    context: &CompositeAlterationContext<'_>,
    definition: &StoredComposite,
    changes: &[CompositeAttributeDefinition],
) -> Result<Vec<StoredCompositeAttribute>, SQLError> {
    let mut dependencies = None;
    let mut prepared = Vec::with_capacity(changes.len());
    for change in changes {
        let attribute = prepare_attribute_type(
            context.types,
            context.attributes.values.types,
            definition,
            change,
        )?;
        if dependencies.is_none() {
            dependencies = Some(catalog_dependencies(
                &context.removal.catalog_removal_context().catalog,
            )?);
        }
        dependencies
            .as_ref()
            .expect("type dependencies")
            .reject_stored_composite_uses(definition.oid, &definition.identity.name)?;
        prepared.push(attribute);
    }
    Ok(prepared)
}

pub(super) fn publish(
    context: &CompositeAlterationContext<'_>,
    original: &StoredComposite,
    definition: &mut StoredComposite,
    changes: Vec<StoredCompositeAttribute>,
) -> Result<(), SQLError> {
    if changes.is_empty() {
        return Ok(());
    }
    let dependents = dependents::Dependents::capture(context, definition.relation_oid, &changes)?;
    for change in changes {
        let current = definition
            .attributes
            .iter_mut()
            .find(|attribute| !attribute.dropped && attribute.name == change.name)
            .ok_or_else(|| {
                uqa_sql::schema::columns::undefined_relation_column(
                    &definition.identity.name,
                    &change.name,
                )
            })?;
        let initial = original
            .attributes
            .iter()
            .find(|attribute| attribute.number == current.number)
            .ok_or_else(|| SQLError::Internal("original composite attribute disappeared".into()))?;
        uqa_sql::schema::columns::type_target::validate_repeated_type_change(
            &change.name,
            &initial.ty,
            &current.ty,
        )?;
        let removal = context.removal.catalog_removal_context();
        let dependencies = catalog_dependencies(&removal.catalog)?;
        reject_field_dependents(
            dependencies.graph(),
            ObjectAddress::column(definition.relation_oid, i32::from(current.number)),
            &change.name,
            |address| dependencies.describe(&removal.catalog, address),
            |address| match dependencies.catalog_object(address) {
                Some(crate::catalog::projection::CatalogObject::ColumnDefault {
                    column, ..
                }) => FieldDependent::ColumnDefault(column),
                Some(crate::catalog::projection::CatalogObject::DomainConstraint { .. }) => {
                    FieldDependent::DomainConstraint
                }
                _ => FieldDependent::Other,
            },
        )?;
        *current = change;
        let before = context.attributes.publication.composite_registry().clone();
        let mut after = before.clone();
        after.insert(definition.identity.qualified_name(), definition.clone());
        crate::catalog::composite_type::publish(context.attributes.publication, &before, after)?;
        context.attributes.changes.catalog_registry_changed();
    }
    dependents.rebuild(context)
}
