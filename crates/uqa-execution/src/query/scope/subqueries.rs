//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cached scalar results and SQL membership semantics for one query arena.

use crate::physical::physical_exec_error;
use crate::{RowSchemaExecution, ScalarExpr};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{expr::RowLookup, SQLError};

#[derive(Clone)]
pub enum ScalarSubqueryCacheEntry {
    Correlated,
    CorrelatedExists(Arc<CachedCorrelatedExists>),
    Materialized(CachedScalarSubquery),
    Membership(Arc<CachedSubqueryMembership>),
    Scalar(Value),
    Exists(bool),
}

pub struct CachedCorrelatedExists {
    pub outer_keys: CorrelatedExistsOuterKeys,
    pub keys: crate::CanonicalRowHashSet,
}

pub enum CorrelatedExistsOuterKeys {
    Direct(Vec<DirectColumnKey>),
    Evaluated(Vec<ScalarExpr>),
}

impl CorrelatedExistsOuterKeys {
    pub fn compile(expressions: Vec<ScalarExpr>) -> Self {
        let direct = expressions
            .iter()
            .map(DirectColumnKey::compile)
            .collect::<Option<Vec<_>>>();
        direct.map_or(Self::Evaluated(expressions), Self::Direct)
    }
}

/// A scalar key expression that can be resolved as a borrowed physical value instead of cloning it through the general expression evaluator.
pub enum DirectColumnKey {
    Column(String),
    Qualified { qualifier: String, column: String },
}

impl DirectColumnKey {
    pub fn compile(expression: &ScalarExpr) -> Option<Self> {
        match expression {
            ScalarExpr::Column(column) => Some(Self::Column(column.clone())),
            ScalarExpr::QualifiedColumn { qualifier, column } => Some(Self::Qualified {
                qualifier: qualifier.clone(),
                column: column.clone(),
            }),
            _ => None,
        }
    }

    pub fn value<'a>(&self, row: &'a dyn RowLookup) -> Option<&'a Value> {
        match self {
            Self::Column(column) => row.column(column),
            Self::Qualified { qualifier, column } => row.qualified_column(qualifier, column),
        }
    }
}

pub struct CachedSubqueryMembership {
    values: parking_lot::Mutex<crate::ExactRowSet>,
    has_column: bool,
    saw_row: bool,
    saw_null: bool,
}

impl CachedSubqueryMembership {
    pub fn contains(&self, needle: &Value) -> Result<Option<bool>, SQLError> {
        if !self.has_column {
            return Ok(Some(false));
        }
        if !matches!(needle, Value::Null)
            && self
                .values
                .lock()
                .contains_values(std::slice::from_ref(needle))
                .map_err(physical_exec_error)?
        {
            return Ok(Some(true));
        }
        Ok(if !self.saw_row {
            Some(false)
        } else if matches!(needle, Value::Null) || self.saw_null {
            None
        } else {
            Some(false)
        })
    }
}

#[derive(Clone)]
pub struct CachedScalarSubquery {
    pub columns: Vec<String>,
    pub rows: crate::SharedSpill,
}

impl CachedScalarSubquery {
    pub fn result(&self) -> Result<crate::SubqueryResult, SQLError> {
        let rows = self
            .rows
            .read_rows()
            .map_err(physical_exec_error)?
            .map(|row| row.map_err(physical_exec_error));
        Ok(crate::SubqueryResult {
            columns: self.columns.clone(),
            rows: Box::new(rows),
        })
    }

    pub fn membership(&self, work_mem_bytes: usize) -> Result<CachedSubqueryMembership, SQLError> {
        let Some(first_column) = self.columns.first() else {
            return Ok(CachedSubqueryMembership {
                values: parking_lot::Mutex::new(crate::ExactRowSet::new(work_mem_bytes)),
                has_column: false,
                saw_row: false,
                saw_null: false,
            });
        };
        let mut values = crate::ExactRowSet::new(work_mem_bytes);
        let mut saw_row = false;
        let mut saw_null = false;
        for batch in self.rows.reader().map_err(physical_exec_error)? {
            let batch = batch.map_err(physical_exec_error)?;
            let position = batch.schema.position(first_column).ok_or_else(|| {
                SQLError::Internal(format!(
                    "cached subquery output column `{first_column}` is missing"
                ))
            })?;
            for row in &batch.rows {
                saw_row = true;
                match batch.schema.view(row).value_at(position) {
                    Some(Value::Null) | None => saw_null = true,
                    Some(value) => {
                        values
                            .insert_values(std::slice::from_ref(value))
                            .map_err(physical_exec_error)?;
                    }
                }
            }
        }
        Ok(CachedSubqueryMembership {
            values: parking_lot::Mutex::new(values),
            has_column: true,
            saw_row,
            saw_null,
        })
    }
}
