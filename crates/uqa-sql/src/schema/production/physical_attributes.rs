//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical scan metadata owns its executor attributes and hidden SQL aliases.

use super::{copy, optional_type};
use crate::{
    ast::InternalColumnRef,
    schema::{ColumnIdentity, RowSchema, SchemaMap},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl RowSchema {
    pub fn with_physical_identity_aliases_with_control<'a>(
        input: &'a Self,
        aliases: impl IntoIterator<Item = (Option<&'a str>, &'a str, usize, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        let mut collations = crate::schema::collations::composition::Selection::new(
            &input.index.cold.collations,
            control,
        )?;
        for (qualifier, column, slot, ty) in aliases {
            assert!(
                slot < input.physical_width(),
                "physical identity alias is outside row width"
            );
            selected.insert_copy((qualifier, column), (slot, ty))?;
            collations.alias(
                (qualifier, column),
                input.inherited_physical_collation(slot),
                ty,
            )?;
        }
        let aliases = SchemaMap::from_produced(
            input
                .index
                .aliases
                .iter()
                .map(|(identity, slot)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })
                .chain(selected.iter().map(|((qualifier, column), (slot, _))| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })),
            control,
        )?;
        let types = SchemaMap::from_produced(
            input
                .index
                .cold
                .aliases
                .iter()
                .map(|(identity, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(selected.iter().map(|((qualifier, column), (_, ty))| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        optional_type(*ty, control)?,
                    ))
                })),
            control,
        )?;
        let mut metadata = copy::metadata_with_identity_maps(
            input,
            collations.finish(control)?,
            aliases,
            types,
            control,
        )?;
        metadata.local_width = None;
        copy::with_metadata(input, metadata, control)
    }

    pub fn append_internal_typed_with_control<'a>(
        input: &'a Self,
        columns: impl IntoIterator<Item = (InternalColumnRef, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        let mut collations = crate::schema::collations::composition::Selection::new(
            &input.index.cold.collations,
            control,
        )?;
        let base = input.physical_width();
        let mut width = base;
        for (column, ty) in columns {
            collations.internal(column, None, ty)?;
            selected.insert_copy(column, (width, ty))?;
            width += 1;
        }
        let mut metadata = copy::metadata(input, control)?;
        metadata.collations = collations.finish(control)?;
        metadata.internal = SchemaMap::from_produced(
            input
                .index
                .executor_attributes
                .iter()
                .map(|(column, slot)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })
                .chain(selected.iter().map(|(column, (slot, _))| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })),
            control,
        )?;
        metadata.internal_types = SchemaMap::from_produced(
            input
                .index
                .cold
                .executor_attribute_types
                .iter()
                .map(|(column, ty)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(selected.iter().map(|(column, (_, ty))| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        optional_type(*ty, control)?,
                    ))
                })),
            control,
        )?;
        metadata.local_width = None;
        copy::with_metadata_width(input, metadata, width, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn physical_scan_attributes_keep_slots_types_collations_and_alias_precedence() {
        let input = RowSchema::with_column_metadata(
            Some("q"),
            [
                ("x".into(), Some(ColumnType::Text), Some(950)),
                ("n".into(), Some(ColumnType::Integer), None),
            ],
        );
        let relation = crate::ast::InternalRelationId::allocate();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let extended = RowSchema::append_internal_typed_with_control(
            &input,
            [
                (relation.column(0), Some(&ColumnType::BigInteger)),
                (relation.column(1), Some(&ColumnType::DoublePrecision)),
            ],
            &control,
        )
        .unwrap();
        assert_eq!(extended.columns(), input.columns());
        assert_eq!(extended.physical_width(), 4);
        assert_eq!(extended.internal_slot(relation.column(0)), Some(2));
        assert_eq!(extended.internal_slot(relation.column(1)), Some(3));
        assert_eq!(extended.column_collation(0), Some(950));
        let aliases = RowSchema::with_physical_identity_aliases_with_control(
            &extended,
            [
                (Some("q"), "hidden", 0, Some(&ColumnType::Text)),
                (Some("q"), "hidden", 3, Some(&ColumnType::DoublePrecision)),
                (Some("q"), "x", 2, Some(&ColumnType::BigInteger)),
                (Some("q"), "text_alias", 0, Some(&ColumnType::Text)),
            ],
            &control,
        )
        .unwrap();
        assert_eq!(aliases.qualified_slot("q", "hidden"), Some(3));
        assert_eq!(aliases.qualified_slot("q", "x"), Some(0));
        assert_eq!(
            aliases.qualified_type("q", "hidden"),
            Some(&ColumnType::DoublePrecision)
        );
        assert_eq!(
            aliases.qualified_type("q", "text_alias"),
            Some(&ColumnType::Text)
        );
        assert_eq!(aliases.internal_slot(relation.column(1)), Some(3));
        assert_eq!(aliases.columns(), input.columns());
        assert_eq!(input.physical_width(), 2);
        let retained = aliases.clone();
        drop(extended);
        let held = memory.used();
        drop(aliases);
        assert_eq!(memory.used(), held);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn physical_scan_aliases_admit_large_names_before_copying_and_release_failures() {
        let input = RowSchema::new(vec!["x".into()]);
        let name = "x".repeat(4 << 20);
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let build = || {
            RowSchema::with_physical_identity_aliases_with_control(
                &input,
                [(Some("q"), name.as_str(), 0, Some(&ColumnType::Integer))],
                &control,
            )
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut failed = None;
        let allocation = allocation_counter::measure(|| failed = Some(build()));
        assert_eq!(failed.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(build().unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        let schema = build().unwrap();
        assert_eq!(schema.qualified_slot("q", &name), Some(0));
        assert!(memory.used() >= name.len());
        drop(schema);
        assert_eq!(memory.used(), 0);
    }
}
