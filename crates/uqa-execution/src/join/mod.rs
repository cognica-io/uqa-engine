//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical relational join operators.

use crate::RowSchemaExecution;
mod canonical_index;
mod direct_index;
mod iteration;
mod nested_loop;
mod output;
use output::JoinOutput;
pub(crate) mod presence;
mod production;
mod row_store;
use iteration::HashJoinState;

use smallvec::SmallVec;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::ast::JoinKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;

use crate::distinct::{encode_typed_non_null_key, EncodedKey};
use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, ProjectedPredicate, RowSchema,
    ScalarExpr, SharedExpressionEvaluator,
};

use crate::spill::MatchFlags;
use canonical_index::{HybridHashIndex, MemoryMatchSummary};
use direct_index::{positional_key_hash, DirectHashIndex, DirectMatches};
use row_store::HybridRowStore;

pub use nested_loop::NestedLoopJoin;

const DEFAULT_JOIN_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

/// One probe batch establishes whether a join needs its other input, without draining the probe or retaining more than its ordinary batch allowance.
fn next_nonempty_batch(
    input: &mut dyn PhysicalOperator,
    demanded: bool,
) -> ExecResult<Option<Batch>> {
    while let Some(batch) = if demanded {
        input.next_with_demand(crate::physical::demand::ONE)?
    } else {
        input.next()?
    } {
        if !batch.rows.is_empty() {
            return Ok(Some(batch));
        }
    }
    Ok(None)
}

/// Equality join backed by a canonical SQL-key hash table. SQL NULL keys never
/// match. An optional residual predicate is evaluated on hash candidates
/// before either side is marked matched, preserving mixed equijoin/non-equality
/// `ON` semantics for every outer-join shape.
pub struct HashJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left_keys: crate::scalar::PreparedExpressions<Produced<Vec<ScalarExpr>>>,
    right_keys: crate::scalar::PreparedExpressions<Produced<Vec<ScalarExpr>>>,
    left_key_types: Vec<Option<uqa_sql::ColumnType>>,
    right_key_types: Vec<Option<uqa_sql::ColumnType>>,
    indeterminate_keys: Option<Produced<Vec<usize>>>,
    left_key_positions: Option<Produced<Vec<usize>>>,
    right_key_positions: Option<Produced<Vec<usize>>>,
    predicate: Option<crate::scalar::PreparedExpressions<Produced<ScalarExpr>>>,
    prepared_predicate: Option<ProjectedPredicate>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: PhysicalRow,
    right_nulls: PhysicalRow,
    schema: RowSchema,
    estimated_cardinality: Option<u64>,
    build_left: bool,
    work_mem_bytes: usize,
    output: JoinOutput,
    streaming_unique: Option<UniqueHashJoinState>,
    streaming: Option<HashJoinState>,
    probe_pending: crate::physical::demand::PendingRows,
    initialized: bool,
    prefetched_left: Option<Batch>,
    right_input_spilled: SpillState,
    hash_index_spilled: SpillState,
    memory: Option<uqa_core::memory::MemoryReservation>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SpillState {
    #[default]
    InMemory,
    Spilled,
}

impl SpillState {
    fn is_spilled(self) -> bool {
        matches!(self, Self::Spilled)
    }
}

impl From<bool> for SpillState {
    fn from(spilled: bool) -> Self {
        if spilled {
            Self::Spilled
        } else {
            Self::InMemory
        }
    }
}

/// State retained while an in-memory unique-key inner join streams its probe
/// side. At most one output row can be produced per probe row, so the join can
/// preserve batch backpressure without a cardinality-sized output buffer.
struct UniqueHashJoinState {
    build_rows: HybridRowStore,
    hash_index: JoinHashIndex,
    build_left: bool,
}

enum JoinHashIndex {
    /// Simple column keys keep only hashes and row positions. Candidate keys
    /// are verified against the original build row slots.
    Direct(DirectHashIndex),
    /// Evaluated expressions and spill fallback retain canonical byte keys.
    Encoded(HybridHashIndex),
}

impl<'a> HashJoin<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "keeps join keys and schema aligned"
    )]
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
    ) -> Self {
        Self::new_with_work_mem(
            left,
            right,
            kind,
            left_keys,
            right_keys,
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
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        Self::new_with_work_mem_and_predicate(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            None,
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keeps join keys and schema aligned"
    )]
    pub fn new_with_work_mem_and_predicate(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        let control = ProductionControl::uncontrolled();
        Self::build_with_owned_expressions(
            left,
            right,
            kind,
            control
                .finish(left_keys, None)
                .expect("ordinary left join keys"),
            control
                .finish(right_keys, None)
                .expect("ordinary right join keys"),
            predicate.map(|predicate| {
                control
                    .finish(predicate, None)
                    .expect("ordinary join predicate")
            }),
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
            None,
            &control,
        )
        .expect("ordinary hash join construction")
    }

    /// Construct a hash join while preparing supported residual predicates
    /// against its composite output schema. Parameters and constant LIKE
    /// patterns are folded exactly once before any candidate row is probed.
    #[expect(
        clippy::too_many_arguments,
        reason = "keeps join keys and schema aligned"
    )]
    pub fn try_new_with_work_mem_and_predicate(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
        params: &[uqa_sql::SQLParam],
    ) -> ExecResult<Self> {
        let control = ProductionControl::uncontrolled();
        Self::build_with_owned_expressions(
            left,
            right,
            kind,
            control
                .finish(left_keys, None)
                .expect("ordinary left join keys"),
            control
                .finish(right_keys, None)
                .expect("ordinary right join keys"),
            predicate.map(|predicate| {
                control
                    .finish(predicate, None)
                    .expect("ordinary join predicate")
            }),
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
            Some(params),
            &control,
        )
        .map_err(Into::into)
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output.spilled
    }

    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled.is_spilled()
    }

    pub fn hash_index_has_spilled(&self) -> bool {
        self.hash_index_spilled.is_spilled()
    }

    pub fn builds_left_input(&self) -> bool {
        self.build_left
    }

    fn next_left_batch(
        &mut self,
        demand: Option<std::num::NonZeroUsize>,
    ) -> ExecResult<Option<Batch>> {
        if let Some(batch) = self.prefetched_left.take() {
            self.probe_pending.retain_batch(batch);
        }
        self.probe_pending.pull(self.left.as_mut(), demand)
    }

    fn key_positions(&self, left: bool) -> Option<&[usize]> {
        if left {
            self.left_key_positions.as_deref()
        } else {
            self.right_key_positions.as_deref()
        }
        .map(Vec::as_slice)
    }

    fn positional_keys(&self, left: bool) -> ExecResult<&[usize]> {
        self.key_positions(left)
            .ok_or_else(|| ExecError::Other("direct join key positions are missing".into()))
    }

    fn key_for_side(
        &self,
        left: bool,
        row: &PhysicalRow,
        schema: &RowSchema,
    ) -> ExecResult<Option<EncodedKey>> {
        let (expressions, types) = if left {
            (&self.left_keys, &self.left_key_types)
        } else {
            (&self.right_keys, &self.right_key_types)
        };
        self.key(expressions, types, self.key_positions(left), row, schema)
    }

    fn rebuild_encoded_index(
        &self,
        rows: &mut HybridRowStore,
        build_left: bool,
        budget_bytes: usize,
    ) -> ExecResult<HybridHashIndex> {
        let schema = rows.schema.clone();
        let mut index = HybridHashIndex::new(budget_bytes);
        for row_index in 0..rows.len() {
            let key =
                rows.with_row(row_index, |row| self.key_for_side(build_left, row, &schema))?;
            if let Some(key) = key {
                index.insert(key, row_index)?;
            }
        }
        Ok(index)
    }

    fn build_side(&mut self, build_left: bool, state_budget: usize) -> ExecResult<()> {
        let row_budget = state_budget / 2;
        let hash_budget = state_budget - row_budget;
        let simple_inner = matches!(self.kind, JoinKind::Inner) && self.predicate.is_none();
        let direct = simple_inner
            && self.key_positions(build_left).is_some()
            && self.key_positions(!build_left).is_some();
        let mut index = if direct {
            JoinHashIndex::Direct(DirectHashIndex::new(hash_budget))
        } else {
            JoinHashIndex::Encoded(HybridHashIndex::new(hash_budget))
        };
        let build = if build_left {
            &mut self.left
        } else {
            &mut self.right
        };
        let mut rows = HybridRowStore::new(build.row_schema().clone(), row_budget);
        build.open()?;
        while let Some(batch) = if build_left {
            self.left.next()?
        } else {
            self.right.next()?
        } {
            for row in batch.rows {
                match &mut index {
                    JoinHashIndex::Direct(index) => {
                        let positions = self.positional_keys(build_left)?;
                        if let Some(hash) = positional_key_hash(
                            index.hasher(),
                            batch.schema.view(&row),
                            positions,
                            self.indeterminate_keys(),
                            self.evaluator.enum_labels(),
                        )? {
                            index.insert(hash, rows.len());
                        }
                    }
                    JoinHashIndex::Encoded(index) => {
                        if let Some(key) = self.key_for_side(build_left, &row, &batch.schema)? {
                            index.insert(key, rows.len())?;
                        }
                    }
                }
                rows.push(row)?;
            }
        }
        if !build_left {
            self.right_input_spilled = rows.has_spilled().into();
        }
        if rows.len() == 0 && matches!(self.kind, JoinKind::Inner) {
            return Ok(());
        }

        if matches!(&index, JoinHashIndex::Direct(direct) if rows.has_spilled() || !direct.is_available())
        {
            drop(index);
            index = JoinHashIndex::Encoded(self.rebuild_encoded_index(
                &mut rows,
                build_left,
                hash_budget,
            )?);
        }
        let unique = match &index {
            JoinHashIndex::Direct(direct) => direct.keys_are_unique(
                &rows,
                &rows.schema,
                self.positional_keys(build_left)?,
                self.evaluator.enum_labels(),
            )?,
            JoinHashIndex::Encoded(index) => {
                simple_inner && !rows.has_spilled() && index.is_memory_unique()
            }
        };
        if let JoinHashIndex::Encoded(index) = &index {
            self.hash_index_spilled = index.has_spilled().into();
        }
        if build_left {
            self.right.open()?;
        } else if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            self.left.open()?;
        }
        if unique {
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: rows,
                hash_index: index,
                build_left,
            });
        } else {
            self.streaming = Some(HashJoinState::new(rows, index, build_left, self.kind)?);
        }
        Ok(())
    }

    fn key(
        &self,
        expressions: &crate::scalar::PreparedExpressions<Produced<Vec<ScalarExpr>>>,
        types: &[Option<uqa_sql::ColumnType>],
        positions: Option<&[usize]>,
        row: &PhysicalRow,
        schema: &RowSchema,
    ) -> ExecResult<Option<EncodedKey>> {
        if let Some(positions) = positions {
            let view = schema.view(row);
            return encode_typed_non_null_key(
                positions.iter().enumerate().map(|(index, position)| {
                    (
                        view.value_at(*position),
                        types.get(index).and_then(Option::as_ref),
                    )
                }),
                self.indeterminate_keys(),
                self.evaluator.enum_labels(),
            );
        }
        let mut values = SmallVec::<[Value; 4]>::with_capacity(expressions.len());
        for expression in expressions.iter() {
            let value = self.evaluator.evaluate_physical_with_function_states(
                expression,
                schema,
                row,
                expressions.calls(),
            )?;
            if matches!(value, Value::Null) {
                return Ok(None);
            }
            values.push(value);
        }
        encode_typed_non_null_key(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (Some(value), types.get(index).and_then(Option::as_ref))),
            self.indeterminate_keys(),
            self.evaluator.enum_labels(),
        )
    }

    fn matches(&self, row: &PhysicalRow) -> ExecResult<bool> {
        if let Some(predicate) = self.prepared_predicate.as_ref() {
            return Ok(predicate.keep_row(&self.schema.view(row))?);
        }
        self.predicate.as_ref().map_or(Ok(true), |predicate| {
            Ok(truthy(
                &self.evaluator.evaluate_physical_with_function_states(
                    predicate,
                    &self.schema,
                    row,
                    predicate.calls(),
                )?,
            )?)
        })
    }

    fn next_streaming_unique(
        &mut self,
        state: &mut UniqueHashJoinState,
        demand: Option<std::num::NonZeroUsize>,
    ) -> ExecResult<Option<Batch>> {
        loop {
            let next = if state.build_left {
                self.probe_pending.pull(self.right.as_mut(), demand)?
            } else {
                self.next_left_batch(demand)?
            };
            let Some(batch) = next else {
                return Ok(None);
            };
            let mut output = Vec::with_capacity(batch.rows.len());
            for probe_row in batch.rows {
                let index = match &state.hash_index {
                    JoinHashIndex::Direct(index) => {
                        let build_positions = self.positional_keys(state.build_left)?;
                        let probe_positions = self.positional_keys(!state.build_left)?;
                        let probe = batch.schema.view(&probe_row);
                        let catalog = self.evaluator.enum_labels();
                        DirectMatches::for_probe(
                            index,
                            probe,
                            probe_positions,
                            self.indeterminate_keys(),
                            catalog,
                        )?
                        .next(
                            index,
                            &state.build_rows,
                            build_positions,
                            probe,
                            probe_positions,
                            catalog,
                        )?
                    }
                    JoinHashIndex::Encoded(index) => {
                        let Some(key) =
                            self.key_for_side(!state.build_left, &probe_row, &batch.schema)?
                        else {
                            continue;
                        };
                        match index.memory_match_summary(&key) {
                            Some(MemoryMatchSummary::Single(index)) => Some(index),
                            _ => None,
                        }
                    }
                };
                let Some(index) = index else { continue };
                let merged = if state.build_left {
                    state.build_rows.with_row(index, |build_row| {
                        Ok(PhysicalRow::concat_right_owned(build_row, probe_row))
                    })?
                } else {
                    state.build_rows.with_row(index, |build_row| {
                        Ok(PhysicalRow::concat_left_owned(probe_row, build_row))
                    })?
                };
                output.push(merged);
            }
            if !output.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), output)));
            }
        }
    }
}

impl HashJoin<'_> {
    fn initialize(&mut self, demanded: bool) -> ExecResult<()> {
        self.output.remaining = None;
        self.left_key_types = self
            .left_keys
            .iter()
            .map(|key| self.evaluator.expression_type(key, self.left.row_schema()))
            .collect::<Result<_, _>>()?;
        self.right_key_types = self
            .right_keys
            .iter()
            .map(|key| self.evaluator.expression_type(key, self.right.row_schema()))
            .collect::<Result<_, _>>()?;
        self.streaming_unique = None;
        self.prefetched_left = None;
        self.output.spilled = false;
        self.right_input_spilled = SpillState::InMemory;
        self.hash_index_spilled = SpillState::InMemory;

        if !self.build_left && !matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            self.left.open()?;
            self.prefetched_left = next_nonempty_batch(self.left.as_mut(), demanded)?;
            if self.prefetched_left.is_none() {
                return Ok(());
            }
        }
        self.build_side(self.build_left, self.work_mem_bytes / 2)
    }

    fn pull(&mut self, demand: Option<std::num::NonZeroUsize>) -> ExecResult<Option<Batch>> {
        if !self.initialized {
            self.initialized = true;
            self.initialize(demand.is_some())?;
        }
        if let Some(mut state) = self.streaming_unique.take() {
            let result = self.next_streaming_unique(&mut state, demand);
            self.streaming_unique = Some(state);
            return result;
        }
        if let Some(mut state) = self.streaming.take() {
            let rows = demand.map_or(crate::batch::DEFAULT_BATCH_SIZE, |rows| {
                rows.get().min(crate::batch::DEFAULT_BATCH_SIZE)
            });
            let result = self.next_streaming(&mut state, rows, demand.is_some());
            self.streaming = Some(state);
            return result;
        }
        self.output.next_pending()
    }
}

impl PhysicalOperator for HashJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.estimated_cardinality
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output.remaining = None;
        self.streaming_unique = None;
        self.streaming = None;
        self.probe_pending.clear();
        self.prefetched_left = None;
        self.initialized = false;
        self.output.spilled = false;
        self.right_input_spilled = SpillState::InMemory;
        self.hash_index_spilled = SpillState::InMemory;
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(None)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(Some(rows))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output.remaining = None;
        self.left_key_types.clear();
        self.right_key_types.clear();
        self.streaming_unique = None;
        self.streaming = None;
        self.probe_pending.clear();
        self.prefetched_left = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right hash-join input")
    }
}

#[cfg(test)]
mod tests;
