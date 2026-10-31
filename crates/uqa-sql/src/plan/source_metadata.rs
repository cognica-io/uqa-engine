//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared metadata of SQL-invisible VALUES inputs, including empty carriers.

use super::SourcePlan;
use crate::{RowSchema, SQLError};

#[cfg(test)]
mod tests;

impl SourcePlan {
    /// Explicit LATERAL permission retained by a range item independently of its parent join.
    pub fn is_explicitly_lateral(&self) -> bool {
        matches!(
            self,
            Self::Values { lateral: true, .. } | Self::Subquery { lateral: true, .. }
        )
    }

    /// Recover an internal VALUES schema from its declarations without sampling a row or exposing SQL-visible attributes.
    pub fn internal_values_schema(&self) -> Result<Option<RowSchema>, SQLError> {
        self.internal_values_schema_with_inference(None)
    }

    /// Fill undeclared captured attributes from their caller scope, retaining explicit declarations and collations unchanged.
    pub fn internal_values_schema_with_inference(
        &self,
        inferred: Option<&RowSchema>,
    ) -> Result<Option<RowSchema>, SQLError> {
        self.internal_values_schema_with_control(
            inferred,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    pub fn internal_values_schema_with_control(
        &self,
        inferred: Option<&RowSchema>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        let Self::Values {
            internal_relation: Some(relation),
            internal_column_types: types,
            internal_column_collations: collations,
            ..
        } = self
        else {
            return Ok(None);
        };
        if !collations.is_empty() && collations.len() != types.len() {
            return Err(SQLError::Internal(
                "internal VALUES collation width does not match its declared attributes".into(),
            ));
        }
        let metadata = types.iter().enumerate().map(|(position, declared)| {
            let ty = declared
                .as_ref()
                .or_else(|| inferred.and_then(|schema| schema.column_type(position)));
            let collation = collations.get(position).copied().flatten().or_else(|| {
                declared
                    .is_none()
                    .then(|| inferred.and_then(|schema| schema.column_collation(position)))
                    .flatten()
            });
            (ty, collation)
        });
        RowSchema::with_internal_relation_metadata_with_control(*relation, metadata, control)
            .map(Some)
    }
}
