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
        for (ty, oid) in types.iter().zip(collations) {
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
            *relation,
            types
                .iter()
                .cloned()
                .enumerate()
                .map(|(position, ty)| (ty, collations.get(position).copied().flatten())),
        )))
    }
}
