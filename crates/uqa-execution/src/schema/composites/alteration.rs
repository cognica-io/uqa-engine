//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite attribute changes under retained type and relation locks, in PostgreSQL's DROP-before-ADD order.

use super::attributes::CompositeAttributeContext;
use crate::catalog::notices::CatalogNotices;
use crate::row_locks::{
    binding::acquire_relation, shared_objects::SharedCatalogLock, RelationLockMode,
};
use crate::schema::deletion::{perform_deletion, required_address, CatalogRemovalInputs};
use crate::schema::table_alteration::binding::TableAlterBindingContext;
use uqa_sql::{
    ast::{CompositeAttributeAddition, CompositeAttributeRemoval},
    catalog::{composite_type::StoredComposite, dependencies::TYPE_CLASS},
    schema::relation_alteration::RelationAlterTarget,
    type_resolution::FunctionTypeResolver,
    SQLError,
};

pub struct CompositeAlterationContext<'a> {
    pub binding: TableAlterBindingContext<'a>,
    pub attributes: CompositeAttributeContext<'a>,
    pub types: &'a dyn FunctionTypeResolver,
    pub sequences: crate::schema::sequences::implicit::ImplicitSequenceContext<'a>,
    pub removal: &'a dyn CatalogRemovalInputs,
    pub notices: &'a dyn CatalogNotices,
}

pub fn alter_attributes(
    context: &CompositeAlterationContext<'_>,
    name: &str,
    removals: &[CompositeAttributeRemoval],
    additions: &[CompositeAttributeAddition],
) -> Result<(), SQLError> {
    let mut definition = bind(context, name)?;
    context.binding.locks.prepare_definition_write()?;
    for removal in removals {
        if !uqa_sql::schema::composites::validate_removed_attribute(&definition, removal)? {
            context
                .notices
                .notice(uqa_sql::schema::columns::missing_drop_column_notice(
                    &definition.identity.name,
                    &removal.name,
                ));
            continue;
        }
        perform_deletion(
            &context.removal.catalog_removal_context(),
            |dependencies| {
                Ok(vec![required_address(
                    dependencies.relation_address(&definition.identity, Some(&removal.name)),
                    || {
                        format!(
                            "composite attribute {}.{}",
                            definition.identity.qualified_name(),
                            removal.name
                        )
                    },
                )?])
            },
            removal.cascade,
        )?;
        definition = resolve(context, name)?;
    }
    super::addition::add_attributes(context, definition, additions)
}

fn resolve(
    context: &CompositeAlterationContext<'_>,
    name: &str,
) -> Result<StoredComposite, SQLError> {
    let target = RelationAlterTarget::resolve(
        context.binding.names.resolve_relation_kind(name)?,
        name,
        false,
        &mut |_| {},
    )?
    .ok_or_else(|| SQLError::UnknownTable(name.to_owned()))?;
    context
        .binding
        .authority
        .ensure_relation_owner_as(&target.relation, target.kind, "table")?;
    uqa_sql::catalog::security::ownership::reject_system_relation_alter(&target.relation)?;
    target.require_kind("composite type")?;
    context
        .attributes
        .publication
        .composite_registry()
        .get(&target.canonical)
        .cloned()
        .ok_or_else(|| SQLError::Internal("composite definition disappeared during binding".into()))
}

fn bind(context: &CompositeAlterationContext<'_>, name: &str) -> Result<StoredComposite, SQLError> {
    loop {
        let initial = resolve(context, name)?;
        // Type lifecycle and dependency deletion use this immutable address as well.
        let type_guard = context.binding.creation.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: TYPE_CLASS,
                oid: initial.oid,
            },
            RelationLockMode::AccessExclusive,
        )?;
        let relation_guard = acquire_relation(
            context.binding.locks,
            &initial.identity.qualified_name(),
            RelationLockMode::AccessExclusive,
            false,
        )?;
        context.binding.locks.refresh_after_wait()?;
        let current = resolve(context, name)?;
        if current.object_id == initial.object_id && current.oid == initial.oid {
            type_guard.retain();
            relation_guard.retain();
            return Ok(current);
        }
    }
}
