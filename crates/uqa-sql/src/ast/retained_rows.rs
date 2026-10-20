//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable SQL metadata for rows retained by an execution scope.

use super::InternalRelationId;
use crate::{RowSchema, SQLError};

/// An executor-owned, SQL-invisible row source. The syntax and plan retain only its identity and complete type descriptor; the execution owner supplies repeatable readers and enforces its memory allowance.
///
/// This descriptor cannot be serialized into a stored statement. Cloning it preserves the source identity and shares the immutable schema, without copying any row values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct RetainedRowSource {
    relation: InternalRelationId,
    schema: RowSchema,
    rows: usize,
}

impl RetainedRowSource {
    pub fn new(
        relation: InternalRelationId,
        schema: RowSchema,
        rows: usize,
    ) -> Result<Self, SQLError> {
        let attributes = schema.internal_columns_with_types();
        if schema.has_sql_bindings()
            || attributes.len() != schema.physical_width()
            || attributes
                .iter()
                .enumerate()
                .any(|(position, (column, slot, _))| {
                    *column != relation.column(position) || *slot != Some(position)
                })
        {
            return Err(SQLError::Internal(
                "retained row source must expose only its own internal physical attributes".into(),
            ));
        }
        Ok(Self {
            relation,
            schema,
            rows,
        })
    }

    pub fn relation(&self) -> InternalRelationId {
        self.relation
    }

    pub fn schema(&self) -> &RowSchema {
        &self.schema
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}
