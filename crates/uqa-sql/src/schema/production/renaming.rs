//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result aliases preserve virtual-name collision rules while admitting each lookup owner.

use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaBuildMetadata, SchemaMap, SchemaSet},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

type VirtualKey<'a> = (Option<&'a str>, &'a str);

struct VirtualNames<'a, 'c> {
    types: ProductionMap<'c, VirtualKey<'a>, (Option<&'a ColumnType>, bool)>,
    unqualified: ProductionMap<'c, &'a str, ()>,
    qualified: ProductionMap<'c, VirtualKey<'a>, ()>,
}

impl RowSchema {
    /// Rename a logical result while preserving binding-only generated columns, ambiguity and open-column behavior.
    pub(crate) fn renamed_with_control(
        input: &Self,
        aliases: &[String],
        qualifier: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let names = virtual_names(input, aliases, qualifier, control)?;
        let metadata = SchemaBuildMetadata {
            binding_only: SchemaMap::from_produced(
                names.types.iter().map(|((qualifier, column), (ty, _))| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        super::optional_type(*ty, control)?,
                    ))
                }),
                control,
            )?,
            extra_ambiguous_unqualified: SchemaSet::from_produced(
                names
                    .unqualified
                    .iter()
                    .map(|(column, ())| super::lease::boxed_text(column, control)),
                control,
            )?,
            extra_ambiguous_qualified: SchemaSet::from_produced(
                names.qualified.iter().map(|((qualifier, column), ())| {
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)
                        .map_err(Into::into)
                }),
                control,
            )?,
            ..SchemaBuildMetadata::empty_with_control(control)
        };
        super::columns_with_metadata(
            qualifier,
            None,
            input
                .columns()
                .iter()
                .enumerate()
                .map(|(position, column)| {
                    (
                        public_name(input, aliases, position, column),
                        input.column_type(position),
                        input.column_collation(position),
                        input.record_metadata(position).cloned(),
                    )
                }),
            input.columns_are_open(None),
            metadata,
            control,
        )
    }
}

fn public_name<'a>(
    input: &'a RowSchema,
    aliases: &'a [String],
    position: usize,
    column: &'a str,
) -> &'a str {
    aliases.get(position).map_or_else(
        || input.public_name(position).unwrap_or(column),
        String::as_str,
    )
}

fn virtual_names<'a, 'c>(
    input: &'a RowSchema,
    aliases: &'a [String],
    qualifier: Option<&'a str>,
    control: &ProductionControl<'c>,
) -> Result<VirtualNames<'a, 'c>, SQLError> {
    let mut visible = ProductionMap::new(*control);
    for (position, column) in input.columns().iter().enumerate() {
        visible.insert_copy(public_name(input, aliases, position, column), ())?;
    }
    let mut names = VirtualNames {
        types: ProductionMap::new(*control),
        unqualified: ProductionMap::new(*control),
        qualified: ProductionMap::new(*control),
    };
    for conflict in [false, true] {
        for (identity, ty) in input.typed_virtual_identities() {
            control.check()?;
            let conflicting = identity.qualifier().map_or_else(
                || {
                    input
                        .index
                        .ambiguous_unqualified
                        .contains(identity.column())
                },
                |_| input.index.ambiguous_qualified.contains(identity),
            );
            if conflicting != conflict {
                continue;
            }
            let qualifiers = qualifier
                .map_or([identity.qualifier(), identity.qualifier()], |qualifier| {
                    [None, Some(qualifier)]
                });
            for selected in &qualifiers[..if qualifier.is_some() { 2 } else { 1 }] {
                let key = (*selected, identity.column());
                let visible_unqualified = visible.get(&identity.column()).is_some();
                let visible_qualified =
                    selected.is_some() && *selected == qualifier && visible_unqualified;
                if conflict {
                    if visible_unqualified
                        || names
                            .types
                            .get(&(None, identity.column()))
                            .is_some_and(|(_, baseline)| *baseline)
                    {
                        names.unqualified.insert_copy(identity.column(), ())?;
                    }
                    if selected.is_some()
                        && (visible_qualified
                            || names.types.get(&key).is_some_and(|(_, baseline)| *baseline))
                    {
                        names.qualified.insert_copy(key, ())?;
                    }
                } else if selected.map_or(visible_unqualified, |_| visible_qualified) {
                    continue;
                }
                if names.types.get(&key).is_none() {
                    names.types.insert_copy(key, (ty, !conflict))?;
                }
            }
        }
    }
    Ok(names)
}
