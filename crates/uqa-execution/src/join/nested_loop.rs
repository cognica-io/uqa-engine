//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested-loop physical join execution.

use uqa_sql::ast::JoinKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;

use super::{
    output_schema, push_output_row, HybridRowStore, MatchFlags, DEFAULT_JOIN_WORK_MEM_BYTES,
};
use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, RowSchema, ScalarExpr,
    SharedExpressionEvaluator, SpillBuffer,
};

/// Nested-loop implementation for arbitrary join predicates and every SQL
/// outer-join shape. Predicate evaluation happens against the merged row, so
/// qualified columns and engine-provided scalar/subquery semantics remain
/// available through the shared expression evaluator.
pub struct NestedLoopJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    predicate: Option<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: PhysicalRow,
    right_nulls: PhysicalRow,
    schema: RowSchema,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
    right_input_spilled: bool,
}

impl<'a> NestedLoopJoin<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
    ) -> Self {
        Self::new_with_work_mem(
            left,
            right,
            kind,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            DEFAULT_JOIN_WORK_MEM_BYTES,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keeps join keys and schema aligned"
    )]
    pub fn new_with_work_mem(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        let schema = output_schema(
            left.row_schema(),
            right.row_schema(),
            &left_nulls,
            &right_nulls,
        );
        let left_nulls = PhysicalRow::nulls(left.row_schema().physical_width());
        let right_nulls = PhysicalRow::nulls(right.row_schema().physical_width());
        Self {
            left,
            right,
            kind,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            work_mem_bytes,
            output: None,
            output_spilled: false,
            right_input_spilled: false,
        }
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }

    /// Whether the repeatable nested-loop build input exceeded its memory
    /// budget and migrated to an indexed temporary row store.
    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled
    }

    fn matches(&self, row: &PhysicalRow) -> ExecResult<bool> {
        match self.predicate.as_ref() {
            None => Ok(true),
            Some(predicate) => Ok(truthy(&self.evaluator.evaluate_physical(
                predicate,
                &self.schema,
                row,
            )?)),
        }
    }
}

impl PhysicalOperator for NestedLoopJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.output_spilled = false;
        self.right_input_spilled = false;

        let right_budget = self.work_mem_bytes / 2;
        let output_budget = self.work_mem_bytes.saturating_sub(right_budget);
        let right_schema = self.right.row_schema().clone();
        let mut right = HybridRowStore::new(right_schema, right_budget);
        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for row in batch.rows {
                right.push(row)?;
            }
        }
        self.right_input_spilled = right.has_spilled();
        let mut matched_right = matches!(self.kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(right.len()))
            .transpose()?;
        let mut output = SpillBuffer::new(output_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for left_row in batch.rows {
                let mut matched_left = false;
                for right_index in 0..right.len() {
                    right.with_row(right_index, |right_row| {
                        let merged = PhysicalRow::concat(&left_row, right_row);
                        if self.matches(&merged)? {
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                            matched_left = true;
                            if let Some(flags) = matched_right.as_mut() {
                                flags.mark(right_index)?;
                            }
                        }
                        Ok(())
                    })?;
                }
                if !matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        PhysicalRow::concat_left_owned(left_row, &self.right_nulls),
                    )?;
                }
            }
        }

        if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            let matched_right = matched_right.as_mut().ok_or_else(|| {
                ExecError::Other("right/full nested-loop join has no match flags".into())
            })?;
            for right_index in 0..right.len() {
                if !matched_right.is_marked(right_index)? {
                    right.with_row(right_index, |right_row| {
                        push_output_row(
                            &mut output,
                            &mut pending,
                            &self.schema,
                            PhysicalRow::concat(&self.left_nulls, right_row),
                        )
                    })?;
                }
            }
        }
        if !pending.is_empty() {
            output.push(Batch::from_physical_rows(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.output
            .as_mut()
            .map_or(Ok(None), |output| output.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right nested-loop join input")
    }
}
