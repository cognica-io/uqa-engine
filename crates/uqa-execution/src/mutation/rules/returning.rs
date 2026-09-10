//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical capture and outer projection of a rule action's RETURNING rows.
use crate::{
    mutation::{
        returning::{
            build_returning_value_row, dml_returning_result, DmlReturningShape,
            ReturningExecutionContext, ReturningValueProjectionRow,
        },
        row_images::RuleRowImage,
    },
    OwnedPhysicalRow,
};
use uqa_core::Value;
use uqa_sql::{SQLError, SQLResult};
pub struct RuleReturningResult {
    current: Vec<Vec<Value>>,
    old: Vec<Vec<Value>>,
    new: Vec<Vec<Value>>,
    contexts: Vec<Option<OwnedPhysicalRow>>,
    affected_rows: u64,
}

impl RuleReturningResult {
    pub fn empty() -> Self {
        Self {
            current: Vec::new(),
            old: Vec::new(),
            new: Vec::new(),
            contexts: Vec::new(),
            affected_rows: 0,
        }
    }

    pub fn project<S: Clone + 'static>(
        self,
        context: ReturningExecutionContext<'_, S>,
        shape: DmlReturningShape<'_, S>,
    ) -> Result<SQLResult, SQLError> {
        if self.current.len() != self.old.len()
            || self.current.len() != self.new.len()
            || self.current.len() != self.contexts.len()
        {
            return Err(SQLError::Internal(
                "rewrite-rule RETURNING images and source contexts have different cardinalities"
                    .into(),
            ));
        }
        let mut rows = Vec::with_capacity(self.current.len());
        for index in 0..self.current.len() {
            rows.push(build_returning_value_row(
                context,
                ReturningValueProjectionRow {
                    table: shape.table,
                    target_qualifier: shape.target_qualifier,
                    current: &self.current[index],
                    old: Some(&self.old[index]),
                    new: Some(&self.new[index]),
                    aliases: shape.aliases,
                    context: self.contexts[index].as_ref(),
                },
                shape.returning,
                shape.params,
                shape.ctes,
            )?);
        }
        dml_returning_result(context, shape, rows, self.affected_rows)
    }
}

pub fn capture_rule_returning_result(
    assignment: &dyn uqa_sql::assignment::AssignmentContext,
    result: SQLResult,
    columns: &[uqa_sql::ast::ColumnDef],
    source_rows: Option<&[RuleRowImage]>,
) -> Result<RuleReturningResult, SQLError> {
    let width = columns.len();
    let image_width = width.saturating_mul(3);
    let expected_width = image_width + usize::from(source_rows.is_some());
    if result.columns.len() != expected_width {
        return Err(SQLError::Internal(format!(
            "rewrite-rule RETURNING provider produced {} columns, expected {}",
            result.columns.len(),
            expected_width
        )));
    }
    let extract = |row: usize, offset: usize| {
        columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                let value = result
                    .value_at(row, offset + position)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING provider lost a positional value".into(),
                        )
                    })?;
                uqa_sql::assignment::conversion::convert_value_to_column_type_with_context(
                    assignment, value, &column.ty,
                )
            })
            .collect::<Result<Vec<_>, SQLError>>()
    };
    let mut current = Vec::with_capacity(result.rows.len());
    let mut old = Vec::with_capacity(result.rows.len());
    let mut new = Vec::with_capacity(result.rows.len());
    let mut contexts = Vec::with_capacity(result.rows.len());
    for row in 0..result.rows.len() {
        current.push(extract(row, 0)?);
        old.push(extract(row, width)?);
        new.push(extract(row, width * 2)?);
        let context = source_rows
            .map(|source_rows| {
                let source_index = match result.value_at(row, image_width) {
                    Some(Value::Int(index)) if *index >= 0 => usize::try_from(*index).map_err(|_| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING source index exceeds usize".into(),
                        )
                    })?,
                    Some(value) => {
                        return Err(SQLError::Internal(format!(
                            "rewrite-rule RETURNING source index is not a non-negative integer: {value:?}"
                        )))
                    }
                    None => {
                        return Err(SQLError::Internal(
                            "rewrite-rule RETURNING provider lost its source index".into(),
                        ))
                    }
                };
                source_rows
                    .get(source_index)
                    .map(|source| source.context.clone())
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING source index is outside the event row set"
                                .into(),
                        )
                    })
            })
            .transpose()?
            .flatten();
        contexts.push(context);
    }
    Ok(RuleReturningResult {
        current,
        old,
        new,
        contexts,
        affected_rows: result.affected_rows,
    })
}
