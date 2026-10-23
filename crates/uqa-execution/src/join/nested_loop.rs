//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested-loop physical join execution.

use uqa_sql::ast::JoinKind;
use uqa_sql::ResultRow;

use super::{output_schema, HybridRowStore, MatchFlags, SpillState, DEFAULT_JOIN_WORK_MEM_BYTES};

mod iteration;
use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, RowSchema, ScalarExpr,
    SharedExpressionEvaluator,
};

/// Nested-loop implementation for arbitrary join predicates and every SQL
/// outer-join shape. Predicate evaluation happens against the merged row, so
/// qualified columns and engine-provided scalar/subquery semantics remain
/// available through the shared expression evaluator.
pub struct NestedLoopJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    predicate: Option<crate::scalar::PreparedExpressions<ScalarExpr>>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: PhysicalRow,
    right_nulls: PhysicalRow,
    schema: RowSchema,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    left_pending: crate::physical::demand::PendingRows,
    right_pending: crate::physical::demand::PendingRows,
    right_rows: Option<HybridRowStore>,
    matched_right: Option<MatchFlags>,
    current_left: Option<PhysicalRow>,
    right_position: u64,
    unmatched_position: u64,
    matched_left: bool,
    left_finished: bool,
    right_state: RightInputState,
    output_spilled: SpillState,
    right_input_spilled: SpillState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RightInputState {
    Unopened,
    Open,
    Exhausted,
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
        let left = super::presence::input(left, matches!(kind, JoinKind::Right | JoinKind::Full));
        let right = super::presence::input(right, matches!(kind, JoinKind::Left | JoinKind::Full));
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
            predicate: predicate.map(crate::scalar::PreparedExpressions::scalar),
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            work_mem_bytes,
            output: None,
            left_pending: crate::physical::demand::PendingRows::default(),
            right_pending: crate::physical::demand::PendingRows::default(),
            right_rows: None,
            matched_right: None,
            current_left: None,
            right_position: 0,
            unmatched_position: 0,
            matched_left: false,
            left_finished: false,
            right_state: RightInputState::Unopened,
            output_spilled: SpillState::InMemory,
            right_input_spilled: SpillState::InMemory,
        }
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled.is_spilled()
    }

    /// Whether the repeatable nested-loop build input exceeded its memory
    /// budget and migrated to an indexed temporary row store.
    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled.is_spilled()
    }
}

impl PhysicalOperator for NestedLoopJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.output_spilled = SpillState::InMemory;
        self.right_input_spilled = SpillState::InMemory;
        self.left_pending.clear();
        self.right_pending.clear();
        self.current_left = None;
        self.right_position = 0;
        self.unmatched_position = 0;
        self.matched_left = false;
        self.left_finished = false;
        self.right_state = RightInputState::Unopened;
        self.right_rows = Some(HybridRowStore::new(
            self.right.row_schema().clone(),
            self.work_mem_bytes / 2,
        ));
        self.matched_right = matches!(self.kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(0, "join"))
            .transpose()?;
        self.left.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(crate::batch::DEFAULT_BATCH_SIZE, false)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(rows.get().min(crate::batch::DEFAULT_BATCH_SIZE), true)
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.current_left = None;
        self.right_rows = None;
        self.matched_right = None;
        self.left_pending.clear();
        self.right_pending.clear();
        let left = self.left.close();
        let right = if self.right_state == RightInputState::Open {
            self.right.close()
        } else {
            Ok(())
        };
        self.right_state = RightInputState::Exhausted;
        crate::physical::with_cleanup(left, right, "close right nested-loop join input")
    }
}
