//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::lease::boxed_text;
use crate::{
    schema::{ColumnIdentity, SchemaBuildMetadata, SchemaMap, SchemaSet},
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(super) struct Indexes {
    pub exact: SchemaMap<Box<str>, usize>,
    pub unqualified: SchemaMap<Box<str>, usize>,
    pub qualified: SchemaMap<ColumnIdentity, usize>,
    pub ambiguous_unqualified: SchemaSet<Box<str>>,
    pub ambiguous_qualified: SchemaSet<ColumnIdentity>,
}

pub(super) fn produce(
    columns: &[String],
    identities: &[ColumnIdentity],
    metadata: &SchemaBuildMetadata,
    control: &ProductionControl<'_>,
) -> Result<Indexes, SQLError> {
    let exact = SchemaMap::from_produced(
        columns.iter().enumerate().map(|(position, name)| {
            Ok((
                boxed_text(name, control)?,
                control.finish(position, control.empty_reservation())?,
            ))
        }),
        control,
    )?;
    // Borrowed scratch records the last logical owner and whether a name occurs more than once, matching ordinary insertion and ambiguity semantics.
    let mut unqualified = ProductionMap::new(*control);
    let mut qualified = ProductionMap::new(*control);
    for (position, identity) in identities.iter().enumerate() {
        control.check()?;
        let duplicate = unqualified.get(&identity.column()).is_some();
        unqualified.insert_copy(identity.column(), (position, duplicate))?;
        if identity.qualifier().is_some() {
            let duplicate = qualified.get(&identity).is_some();
            qualified.insert_copy(identity, (position, duplicate))?;
        }
    }
    let ambiguous_unqualified = SchemaSet::from_produced(
        unqualified
            .iter()
            .filter(|(name, (_, duplicate))| {
                *duplicate
                    && (!metadata.exact_unqualified_precedence
                        || !identities.iter().any(|identity| {
                            identity.qualifier().is_none() && identity.column() == **name
                        }))
            })
            .map(|(name, _)| boxed_text(name, control))
            .chain(
                metadata
                    .extra_ambiguous_unqualified
                    .iter()
                    .map(|name| boxed_text(name, control)),
            ),
        control,
    )?;
    let ambiguous_qualified = SchemaSet::from_produced(
        qualified
            .iter()
            .filter(|(_, (_, duplicate))| *duplicate)
            .map(|(identity, _)| identity.clone_with_control(control).map_err(Into::into))
            .chain(
                metadata
                    .extra_ambiguous_qualified
                    .iter()
                    .map(|identity| identity.clone_with_control(control).map_err(Into::into)),
            ),
        control,
    )?;
    let unqualified = SchemaMap::from_produced(
        unqualified.iter().map(|(name, (position, _))| {
            Ok((
                boxed_text(name, control)?,
                control.finish(*position, control.empty_reservation())?,
            ))
        }),
        control,
    )?;
    let qualified = SchemaMap::from_produced(
        qualified.iter().map(|(identity, (position, _))| {
            Ok((
                identity.clone_with_control(control)?,
                control.finish(*position, control.empty_reservation())?,
            ))
        }),
        control,
    )?;
    Ok(Indexes {
        exact,
        unqualified,
        qualified,
        ambiguous_unqualified,
        ambiguous_qualified,
    })
}
