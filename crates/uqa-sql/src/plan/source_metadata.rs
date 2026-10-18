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
    /// Recover an internal VALUES schema from its declarations without sampling a row or exposing SQL-visible attributes.
    pub fn internal_values_schema(&self) -> Result<Option<RowSchema>, SQLError> {
        self.internal_values_schema_with_inference(None)
    }

    /// Fill undeclared captured attributes from their caller scope, retaining explicit declarations and collations unchanged.
    pub fn internal_values_schema_with_inference(
        &self,
        inferred: Option<&RowSchema>,
    ) -> Result<Option<RowSchema>, SQLError> {
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
        let metadata = types
            .iter()
            .enumerate()
            .map(|(position, declared)| {
                let inferred_type = inferred
                    .and_then(|schema| schema.column_types().get(position))
                    .cloned()
                    .flatten();
                let ty = declared.clone().or(inferred_type);
                let collation = collations.get(position).copied().flatten().or_else(|| {
                    declared
                        .is_none()
                        .then(|| inferred.and_then(|schema| schema.column_collation(position)))
                        .flatten()
                });
                (ty, collation)
            })
            .collect::<Vec<_>>();
        for (ty, oid) in &metadata {
            if oid.is_some_and(|oid| oid != 0)
                && !ty
                    .as_ref()
                    .is_some_and(crate::schema::composites::type_is_collatable)
            {
                return Err(SQLError::Internal(
                    "internal VALUES collation requires a collatable attribute type".into(),
                ));
            }
        }
        Ok(Some(RowSchema::with_internal_relation_metadata(
            *relation, metadata,
        )))
    }
}
