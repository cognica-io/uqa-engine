//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-operation input validation and physical construction.

use super::{
    set_operation_types, AlignSchema, ColumnEvaluator, DirectionalAppendPosition,
    ExternalSetOperation, RowCursor,
};
use crate::physical::construction::OperatorBuild;
use crate::{
    ExecError, ExecResult, ExternalSort, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator, SortKey,
};
use std::sync::Arc;
use uqa_sql::ast::{ColumnType, SetOpKind};

impl<'a> AlignSchema<'a> {
    pub(super) fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        output: Vec<String>,
        output_types: &[Option<ColumnType>],
    ) -> ExecResult<Self> {
        let (child, (schema, coercions)) = OperatorBuild::run(child, |child| {
            let source = child.schema().to_vec();
            if source.len() != output.len() {
                return Err(crate::physical::physical_exec_error(ExecError::Other(
                    format!(
                        "set-operation inputs have different widths: {} and {}",
                        output.len(),
                        source.len()
                    ),
                )));
            }
            if output.len() != output_types.len() {
                return Err(crate::physical::physical_exec_error(ExecError::Other(
                    format!(
                        "set-operation output type width {} does not match input width {}",
                        output_types.len(),
                        output.len()
                    ),
                )));
            }
            let coercions = child
                .row_schema()
                .column_types()
                .iter()
                .zip(output_types)
                .map(|(source, target)| {
                    target
                        .as_ref()
                        .filter(|target| source.as_ref() != Some(*target))
                        .cloned()
                })
                .collect();
            let schema = RowSchema::with_types(output, output_types.to_vec());
            Ok((schema, coercions))
        })?;
        Ok(Self {
            input_pending: crate::physical::demand::PendingRows::default(),
            child,
            schema,
            coercions,
        })
    }
}

impl<'a> ExternalSetOperation<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        let (left, right, output_types) = OperatorBuild::pair(left, right, |left, right| {
            set_operation_types(left.row_schema(), right.row_schema())
                .map_err(crate::physical::physical_exec_error)
        })?;
        Self::new_with_types(left, right, kind, all, output_types, work_mem_bytes)
    }

    pub fn new_with_evaluator(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        work_mem_bytes: usize,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> ExecResult<Self> {
        let (left, right, output_types) = OperatorBuild::pair(left, right, |left, right| {
            set_operation_types(left.row_schema(), right.row_schema())
                .map_err(crate::physical::physical_exec_error)
        })?;
        Self::new_with_types_and_evaluator(
            left,
            right,
            kind,
            all,
            output_types,
            work_mem_bytes,
            evaluator,
        )
    }

    pub fn new_with_types(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        output_types: Vec<Option<ColumnType>>,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        Self::new_with_types_and_evaluator(
            left,
            right,
            kind,
            all,
            output_types,
            work_mem_bytes,
            Arc::new(ColumnEvaluator),
        )
    }

    /// Construct a set operation whose ordinary forward pulls remain one-row incremental when a scroll materialization boundary wraps the complete operation.
    pub fn new_directional_with_types(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        output_types: Vec<Option<ColumnType>>,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        let mut operation =
            Self::new_with_types(left, right, kind, all, output_types, work_mem_bytes)?;
        operation.incremental_union_all = true;
        Ok(operation)
    }

    pub fn new_with_types_and_evaluator(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        output_types: Vec<Option<ColumnType>>,
        work_mem_bytes: usize,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> ExecResult<Self> {
        let output = left.schema().to_vec();
        let (left, right, ()) = OperatorBuild::pair(left, right, |left, right| {
            left.attach(|input| {
                AlignSchema::new(input, output.clone(), &output_types)
                    .map(|input| Box::new(input) as Box<dyn PhysicalOperator + 'a>)
                    .map_err(crate::physical::physical_exec_error)
            })?;
            right.attach(|input| {
                AlignSchema::new(input, output.clone(), &output_types)
                    .map(|input| Box::new(input) as Box<dyn PhysicalOperator + 'a>)
                    .map_err(crate::physical::physical_exec_error)
            })
        })?;
        let (left, right) = if matches!((kind, all), (SetOpKind::Union, true)) {
            (left, right)
        } else {
            let keys = output
                .iter()
                .map(|column| SortKey {
                    expr: ScalarExpr::Column(column.clone().into()),
                    descending: false,
                    nulls_first: Some(true),
                })
                .collect::<Vec<_>>();
            // Both merge inputs are live concurrently. Split the configured
            // budget so their encoded in-memory runs cannot each claim it.
            let per_input = (work_mem_bytes / 2).max(1);
            (
                Box::new(
                    ExternalSort::new(left, keys.clone(), evaluator.clone(), None, per_input)
                        .with_equality_keys(),
                ) as Box<dyn PhysicalOperator + 'a>,
                Box::new(
                    ExternalSort::new(right, keys, Arc::clone(&evaluator), None, per_input)
                        .with_equality_keys(),
                ) as Box<dyn PhysicalOperator + 'a>,
            )
        };
        Ok(Self {
            left: RowCursor::new(left),
            right: RowCursor::new(right),
            evaluator,
            kind,
            all,
            schema: RowSchema::with_types(output, output_types),
            left_group: None,
            right_group: None,
            pending_row: None,
            pending_count: 0,
            output_bytes: work_mem_bytes,
            union_all_left_done: false,
            incremental_union_all: false,
            directional_position: DirectionalAppendPosition::BeforeFirst,
        })
    }
}
