//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ColumnIdentity, ColumnType, JoinOutputSource, RowSchema};
use crate::{schema::production::remapping, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl JoinOutputSource {
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (source, memory) = match self {
            Self::Input(position) => (Self::Input(*position), control.empty_reservation()),
            Self::Cast { input, ty } => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                (Self::Cast { input: *input, ty }, memory)
            }
            Self::Coalesce { left, right, ty } => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                (
                    Self::Coalesce {
                        left: *left,
                        right: *right,
                        ty,
                    },
                    memory,
                )
            }
        };
        Ok(control.finish(source, memory)?)
    }
    fn selected_type<'a>(&'a self, input: &'a RowSchema) -> Option<&'a ColumnType> {
        match self {
            Self::Input(position) => input.column_type(*position),
            Self::Cast { ty, .. } | Self::Coalesce { ty, .. } => Some(ty),
        }
    }
    fn selected_collation(&self, schema: &RowSchema) -> Option<u32> {
        use crate::catalog::collations::derivation::Derivation;

        let derive = |position| {
            schema
                .column_collation(position)
                .map_or(Derivation::None, |oid| {
                    Derivation::column(oid, schema.column_type(position))
                })
        };
        let selected = match self {
            Self::Input(position) => return schema.inherited_column_collation(*position),
            Self::Cast { input, ty } => derive(*input).coerce(schema.column_type(*input), ty),
            Self::Coalesce { left, right, ty } => derive(*left)
                .coerce(schema.column_type(*left), ty)
                .combine(derive(*right).coerce(schema.column_type(*right), ty)),
        };
        self.selected_type(schema)
            .filter(|ty| crate::schema::composites::type_is_collatable(ty))
            .map(|_| u32::try_from(selected.oid()).expect("collation identity"))
    }

    fn validate(&self, width: usize) -> Result<(), SQLError> {
        let message = match self {
            Self::Input(position) if *position >= width => {
                format!("join output input position {position} is outside width {width}")
            }
            Self::Cast { input, .. } if *input >= width => {
                format!("join output cast position {input} is outside width {width}")
            }
            Self::Coalesce { left, right, .. } if *left >= width || *right >= width => format!(
                "join output coalesce positions ({left}, {right}) are outside width {width}"
            ),
            _ => return Ok(()),
        };
        Err(SQLError::Internal(message))
    }
}

/// Compile only surviving physical metadata and unique computed descriptors. The input layout and intermediate anonymous width remain borrowed throughout construction.
pub fn compile_layout_with_control(
    input: &RowSchema,
    columns: &[(String, ColumnIdentity, JoinOutputSource)],
    aliases: &[(ColumnIdentity, JoinOutputSource)],
    control: &ProductionControl<'_>,
) -> Result<(RowSchema, Produced<Vec<JoinOutputSource>>), SQLError> {
    let mut computed = ProductionVec::<JoinOutputSource>::new(*control);
    for source in columns
        .iter()
        .map(|(_, _, source)| source)
        .chain(aliases.iter().map(|(_, source)| source))
    {
        control.check()?;
        source.validate(input.len())?;
        if !matches!(source, JoinOutputSource::Input(_)) && !computed.contains(source) {
            computed.push_produced(source.clone_with_control(control)?)?;
        }
    }
    let source_position = |source: &JoinOutputSource| match source {
        JoinOutputSource::Input(position) => input
            .physical_slot(*position)
            .expect("validated join output input position has a physical slot"),
        JoinOutputSource::Cast { .. } | JoinOutputSource::Coalesce { .. } => {
            input.physical_width()
                + computed
                    .iter()
                    .position(|candidate| candidate == source)
                    .expect("computed join output source was registered")
        }
    };
    let schema = remapping::physical(
        input,
        columns.iter().map(|(name, identity, source)| {
            (
                name.as_str(),
                identity,
                source_position(source),
                source.selected_type(input),
                source.selected_collation(input),
            )
        }),
        aliases.iter().map(|(identity, source)| {
            (
                identity,
                source_position(source),
                source.selected_type(input),
                source.selected_collation(input),
            )
        }),
        input.physical_width() + computed.len(),
        control,
    )?;
    Ok((schema, computed.finish()?))
}
