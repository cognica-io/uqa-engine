//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Add composite attributes under retained relation/type locks and the existing statement transaction.

use super::alteration::CompositeAlterationContext;
use crate::catalog::composite_type;
use uqa_sql::{
    ast::{
        AutoIncrement, ColumnDef, CompositeAttributeAddition, RelationPersistence,
        SequenceOwnership,
    },
    catalog::composite_type::StoredComposite,
    expr::composites::AttributeChange,
    SQLError,
};

pub(super) fn add_attributes(
    context: &CompositeAlterationContext<'_>,
    mut definition: StoredComposite,
    attributes: &[CompositeAttributeAddition],
) -> Result<(), SQLError> {
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
