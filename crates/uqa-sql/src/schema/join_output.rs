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
    let (schema, computed) = compile_layout_with_control(
        input,
        columns,
        aliases,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map_err(|error| match error {
        crate::SQLError::Internal(message) => SchemaLayoutError(message),
        error => SchemaLayoutError(error.to_string()),
    })?;
    Ok((
        schema,
        computed
            .into_uncontrolled()
            .expect("ordinary JOIN output has no reservation"),
    ))
}

mod production;
pub use production::compile_layout_with_control;

pub fn source_type(source: &JoinOutputSource) -> Option<ColumnType> {
    match source {
        JoinOutputSource::Input(_) => None,
        JoinOutputSource::Cast { ty, .. } | JoinOutputSource::Coalesce { ty, .. } => {
            Some(ty.clone())
        }
    }
}
