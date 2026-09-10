//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static layouts shared by qualified joins and physical output shaping.

use super::SchemaLayoutError;
use crate::{ColumnIdentity, ColumnType, RowSchema};

/// Source of one visible or hidden join-output identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutputSource {
    /// Reuse an existing logical input position without copying its value.
    Input(usize),
    /// Apply an implicit binder-selected coercion to one input position.
    Cast { input: usize, ty: ColumnType },
    /// SQL `COALESCE(left::type, right::type)` over two logical input
    /// positions. This is required only for a merged column of `FULL JOIN`.
    Coalesce {
        left: usize,
        right: usize,
        ty: ColumnType,
    },
}

pub fn compile_layout(
    input: &RowSchema,
    columns: &[(String, ColumnIdentity, JoinOutputSource)],
    aliases: &[(ColumnIdentity, JoinOutputSource)],
) -> Result<(RowSchema, Vec<JoinOutputSource>), SchemaLayoutError> {
    let input_width = input.len();
    let mut computed = Vec::<JoinOutputSource>::new();
    for source in columns
        .iter()
        .map(|(_, _, source)| source)
        .chain(aliases.iter().map(|(_, source)| source))
    {
        match source {
            JoinOutputSource::Input(position) if *position >= input_width => {
                return Err(SchemaLayoutError(format!(
                    "join output input position {position} is outside width {input_width}"
                )));
            }
            JoinOutputSource::Cast { input, .. } if *input >= input_width => {
                return Err(SchemaLayoutError(format!(
                    "join output cast position {input} is outside width {input_width}"
                )));
            }
            JoinOutputSource::Coalesce { left, right, .. }
                if *left >= input_width || *right >= input_width =>
            {
                return Err(SchemaLayoutError(format!(
                    "join output coalesce positions ({left}, {right}) are outside width {input_width}"
                )));
            }
            source @ (JoinOutputSource::Cast { .. } | JoinOutputSource::Coalesce { .. }) => {
                if !computed.contains(source) {
                    computed.push(source.clone());
                }
            }
            JoinOutputSource::Input(_) => {}
        }
    }

    let computed_types = computed.iter().map(source_type).collect::<Vec<_>>();
    let intermediate = RowSchema::append_hidden_typed(input, &computed_types);
    let source_position = |source: &JoinOutputSource| -> usize {
        match source {
            JoinOutputSource::Input(position) => input
                .physical_slot(*position)
                .expect("validated join output input position has a physical slot"),
            JoinOutputSource::Cast { .. } | JoinOutputSource::Coalesce { .. } => {
                let index = computed
                    .iter()
                    .position(|candidate| candidate == source)
                    .expect("computed join output source was registered");
                input.physical_width() + index
            }
        }
    };
    let columns = columns
        .iter()
        .map(|(name, identity, source)| {
            let ty = match source {
                JoinOutputSource::Input(position) => intermediate.column_type(*position).cloned(),
                JoinOutputSource::Cast { .. } | JoinOutputSource::Coalesce { .. } => {
                    source_type(source)
                }
            };
            (name.clone(), identity.clone(), source_position(source), ty)
        })
        .collect::<Vec<_>>();
    let aliases = aliases
        .iter()
        .map(|(name, source)| {
            let ty = match source {
                JoinOutputSource::Input(position) => input.column_type(*position).cloned(),
                JoinOutputSource::Cast { .. } | JoinOutputSource::Coalesce { .. } => {
                    source_type(source)
                }
            };
            (name.clone(), source_position(source), ty)
        })
        .collect::<Vec<_>>();
    let schema = RowSchema::remap_typed_physical_identities(&intermediate, &columns, &aliases);
    Ok((schema, computed))
}

pub fn source_type(source: &JoinOutputSource) -> Option<ColumnType> {
    match source {
        JoinOutputSource::Input(_) => None,
        JoinOutputSource::Cast { ty, .. } | JoinOutputSource::Coalesce { ty, .. } => {
            Some(ty.clone())
        }
    }
}
