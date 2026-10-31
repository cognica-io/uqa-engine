//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-image names share selected target metadata while their hidden physical positions remain distinct.

use super::{
    copy, inline, optional_type, ColumnIdentity, Layout, ProductionControl, RowSchema, SQLError,
    SchemaMap, SchemaSet,
};

impl RowSchema {
    pub fn with_row_image_aliases_with_control(
        target: &Self,
        composite_width: usize,
        old: &str,
        new: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let width = target.len();
        assert_eq!(
            width,
            target.physical_width(),
            "RETURNING target must be positional"
        );
        assert!(
            composite_width <= width,
            "RETURNING user columns must belong to the target"
        );
        let mut layout = Layout::new(control);
        layout.append(target, 0, None, control)?;
        let mut metadata = copy::metadata(target, control)?;
        metadata.wildcard_hidden = SchemaSet::from_produced(
            (composite_width..width).map(|slot| inline(slot, control)),
            control,
        )?;
        let selected = || {
            target
                .columns()
                .iter()
                .enumerate()
                .flat_map(|(position, column)| {
                    [
                        (old, column.as_str(), width + position, position),
                        (new, column.as_str(), width * 2 + position, position),
                    ]
                })
        };
        metadata.aliases = SchemaMap::from_produced(
            target
                .index
                .aliases
                .iter()
                .map(|(identity, slot)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        inline(*slot, control)?,
                    ))
                })
                .chain(selected().map(|(qualifier, column, slot, _)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(Some(qualifier), column, control)?,
                        inline(slot, control)?,
                    ))
                })),
            control,
        )?;
        metadata.alias_types = SchemaMap::from_produced(
            target
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
                .chain(selected().map(|(qualifier, column, _, position)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(Some(qualifier), column, control)?,
                        optional_type(target.column_type(position), control)?,
                    ))
                })),
            control,
        )?;
        let mut collations = crate::schema::collations::composition::Selection::new(
            &target.index.cold.collations,
            control,
        )?;
        for (qualifier, column, _, position) in selected() {
            collations.alias(
                (Some(qualifier), column),
                target.column_collation(position),
                target.column_type(position),
            )?;
        }
        metadata.collations = collations.finish(control)?;
        layout.finish(
            width
                .checked_mul(3)
                .ok_or_else(|| SQLError::Internal("RETURNING row width overflow".into()))?,
            metadata,
            control,
        )
    }
}
