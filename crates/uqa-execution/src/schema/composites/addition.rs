//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Add composite attributes under retained relation/type locks and the existing statement transaction.

use super::attributes::CompositeAttributeContext;
use crate::catalog::composite_type;
use crate::row_locks::{
    binding::acquire_relation, shared_objects::SharedCatalogLock, RelationLockMode,
};
use crate::schema::table_alteration::binding::TableAlterBindingContext;
use uqa_sql::{
    ast::{
        AutoIncrement, ColumnDef, CompositeAttributeAddition, RelationPersistence,
        SequenceOwnership,
    },
    catalog::{composite_type::StoredComposite, dependencies::TYPE_CLASS},
    expr::composites::AttributeChange,
    schema::relation_alteration::RelationAlterTarget,
    type_resolution::FunctionTypeResolver,
    SQLError,
};

pub struct CompositeAdditionContext<'a> {
    pub binding: TableAlterBindingContext<'a>,
    pub attributes: CompositeAttributeContext<'a>,
    pub types: &'a dyn FunctionTypeResolver,
    pub sequences: crate::schema::sequences::implicit::ImplicitSequenceContext<'a>,
}

pub fn add_attributes(
    context: &CompositeAdditionContext<'_>,
    name: &str,
    attributes: &[CompositeAttributeAddition],
) -> Result<(), SQLError> {
    let mut definition = bind(context, name)?;
    context.binding.locks.prepare_definition_write()?;
    let mut serial_columns = Vec::new();
    let canonical = definition.identity.qualified_name();
    for attribute in attributes {
        let prepared = uqa_sql::schema::composites::prepare_added_attribute(
            context.types,
            context.attributes.values.types,
            &definition,
            attribute,
        )?;
        if attribute.declaration.serial {
            let mut column = ColumnDef::nullable(&prepared.name, prepared.ty.clone());
            column.auto_increment = Some(AutoIncrement::serial());
            crate::schema::sequences::implicit::materialize_implicit_sequences(
                &context.sequences,
                "ALTER TYPE",
                &canonical,
                std::slice::from_mut(&mut column),
                RelationPersistence::Permanent,
            )?;
            serial_columns.push(column);
        }
        let rebuild = super::values::rewrite_composite_values(
            &context.attributes.values,
            definition.oid,
            &AttributeChange::Add(prepared.name.clone()),
        )?;
        definition.attributes.push(prepared);
        let before = context.attributes.publication.composite_registry().clone();
        let mut after = before.clone();
        after.insert(definition.identity.qualified_name(), definition.clone());
        composite_type::publish(context.attributes.publication, &before, after)?;
        context.attributes.changes.catalog_registry_changed();
        super::values::rebuild_indexes(&context.attributes.values, rebuild)?;
    }
    // PostgreSQL attaches implicit ownership only after every ADD has run. Composite relations cannot own a sequence; that error rolls back the whole statement.
    for column in serial_columns {
        let sequence = column
            .auto_increment
            .as_ref()
            .and_then(|auto| auto.sequence.as_deref())
            .ok_or_else(|| SQLError::Internal("implicit attribute sequence disappeared".into()))?;
        uqa_sql::schema::sequences::ownership::bind_sequence_owner(
            context.sequences.owners,
            sequence,
            &SequenceOwnership::Column {
                table: canonical.clone(),
                column: column.name,
            },
        )?;
    }
    Ok(())
}

fn resolve(
    context: &CompositeAdditionContext<'_>,
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

fn bind(context: &CompositeAdditionContext<'_>, name: &str) -> Result<StoredComposite, SQLError> {
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
