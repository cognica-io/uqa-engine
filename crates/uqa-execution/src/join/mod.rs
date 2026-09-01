//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical relational join operators.

mod canonical_index;
mod direct_index;
mod nested_loop;
mod row_store;

use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::ast::JoinKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;

use crate::distinct::{encode_non_null_key, EncodedKey};
use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow, ProjectedPredicate, RowSchema,
    ScalarExpr, SharedExpressionEvaluator, SpillBuffer,
};

use canonical_index::{HybridHashIndex, MatchFlags, MemoryMatchSummary};
use direct_index::{direct_unique_match, positional_key_hash, DirectHashIndex};
use row_store::HybridRowStore;

pub use nested_loop::NestedLoopJoin;

#[cfg(test)]
use canonical_index::{stable_hash, DiskHashIndex, HASH_BUCKETS};

const DEFAULT_JOIN_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

fn output_schema(
    left: &RowSchema,
    right: &RowSchema,
    left_nulls: &ResultRow,
    right_nulls: &ResultRow,
) -> RowSchema {
    RowSchema::join(
        left,
        right,
        left_nulls.keys().chain(right_nulls.keys()).cloned(),
    )
}

fn push_output_row(
    output: &mut SpillBuffer,
    pending: &mut Vec<PhysicalRow>,
    schema: &RowSchema,
    row: PhysicalRow,
) -> ExecResult<()> {
    pending.push(row);
    if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
        output.push(Batch::from_physical_rows(
            schema.clone(),
            std::mem::take(pending),
        ))?;
        pending.reserve(crate::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

fn join_io_error(operation: &str, error: impl std::fmt::Display) -> ExecError {
    ExecError::Other(format!("join spill {operation}: {error}"))
}

fn simple_key_positions(schema: &RowSchema, expressions: &[ScalarExpr]) -> Option<Vec<usize>> {
    expressions
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Column(column) => schema.position(column),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                schema.qualified_position(qualifier, column)
            }
            _ => None,
        })
        .collect()
}

/// Equality join backed by a canonical SQL-key hash table. SQL NULL keys never
/// match. An optional residual predicate is evaluated on hash candidates
/// before either side is marked matched, preserving mixed equijoin/non-equality
/// `ON` semantics for every outer-join shape.
pub struct HashJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left_keys: Vec<ScalarExpr>,
    right_keys: Vec<ScalarExpr>,
    left_key_positions: Option<Vec<usize>>,
    right_key_positions: Option<Vec<usize>>,
    predicate: Option<ScalarExpr>,
    prepared_predicate: Option<ProjectedPredicate>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: PhysicalRow,
    right_nulls: PhysicalRow,
    schema: RowSchema,
    estimated_cardinality: Option<u64>,
    build_left: bool,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    streaming_unique: Option<UniqueHashJoinState>,
    output_spilled: SpillState,
    right_input_spilled: SpillState,
    hash_index_spilled: SpillState,
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
    hash_index: UniqueHashIndex,
    build_left: bool,
}

enum UniqueHashIndex {
    /// Simple column keys keep only hashes and row positions. Candidate keys
    /// are verified against the original build row slots.
    Direct(DirectHashIndex),
    /// Evaluated expressions and spill fallback retain canonical byte keys.
    Encoded(HybridHashIndex),
}

impl<'a> HashJoin<'a> {
    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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
        let left_key_positions = simple_key_positions(left.row_schema(), &left_keys);
        let right_key_positions = simple_key_positions(right.row_schema(), &right_keys);
        let schema = output_schema(
            left.row_schema(),
            right.row_schema(),
            &left_nulls,
            &right_nulls,
        );
        let left_nulls = PhysicalRow::nulls(left.row_schema().physical_width());
        let right_nulls = PhysicalRow::nulls(right.row_schema().physical_width());
        let left_cardinality = left.estimated_cardinality();
        let right_cardinality = right.estimated_cardinality();
        let build_left = matches!(kind, JoinKind::Inner)
            && left_cardinality
                .zip(right_cardinality)
                .is_some_and(|(left, right)| left < right);
        let estimated_cardinality = left_cardinality
            .zip(right_cardinality)
            .map(|(left, right)| match kind {
                JoinKind::Inner => left.max(right),
                JoinKind::Left => left,
                JoinKind::Right => right,
                JoinKind::Full => left.saturating_add(right),
                JoinKind::Cross => left.saturating_mul(right),
            });
        let prepared_predicate = predicate.as_ref().and_then(|predicate| {
            ProjectedPredicate::compile_with_schema(predicate, &schema, &[])
                .ok()
                .flatten()
        });
        Self {
            left,
            right,
            kind,
            left_keys,
            right_keys,
            left_key_positions,
            right_key_positions,
            predicate,
            prepared_predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            estimated_cardinality,
            build_left,
            work_mem_bytes,
            output: None,
            streaming_unique: None,
            output_spilled: SpillState::InMemory,
            right_input_spilled: SpillState::InMemory,
            hash_index_spilled: SpillState::InMemory,
        }
    }

    /// Construct a hash join while preparing supported residual predicates
    /// against its composite output schema. Parameters and constant LIKE
    /// patterns are folded exactly once before any candidate row is probed.
    #[allow(clippy::too_many_arguments)]
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
        let mut join = Self::new_with_work_mem_and_predicate(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
        );
        join.prepared_predicate = join
            .predicate
            .as_ref()
            .map(|predicate| {
                ProjectedPredicate::compile_with_schema(predicate, &join.schema, params)
            })
            .transpose()?
            .flatten();
        Ok(join)
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled.is_spilled()
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

    fn rebuild_encoded_index(
        &self,
        rows: &mut HybridRowStore,
        expressions: &[ScalarExpr],
        positions: &[usize],
        budget_bytes: usize,
    ) -> ExecResult<HybridHashIndex> {
        let schema = rows.schema.clone();
        let mut index = HybridHashIndex::new(budget_bytes);
        for row_index in 0..rows.len() {
            let key = rows.with_row(row_index, |row| {
                self.key(expressions, Some(positions), row, &schema)
            })?;
            if let Some(key) = key {
                index.insert(key, row_index)?;
            }
        }
        Ok(index)
    }

    fn open_build_left(&mut self, state_budget: usize, output_budget: usize) -> ExecResult<()> {
        debug_assert!(matches!(self.kind, JoinKind::Inner));
        let left_budget = state_budget / 2;
        let hash_budget = state_budget.saturating_sub(left_budget);
        let left_schema = self.left.row_schema().clone();
        let mut left = HybridRowStore::new(left_schema, left_budget);
        let direct_positions = self
            .predicate
            .is_none()
            .then_some(())
            .and(self.left_key_positions.as_deref())
            .zip(self.right_key_positions.as_deref());
        let mut direct_index = direct_positions.map(|_| DirectHashIndex::new(hash_budget));
        let mut encoded_index = direct_index
            .is_none()
            .then(|| HybridHashIndex::new(hash_budget));
        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for row in batch.rows {
                let index = left.len();
                if let (Some(direct), Some((positions, _))) =
                    (direct_index.as_mut(), direct_positions)
                {
                    if let Some(hash) =
                        positional_key_hash(direct.hasher(), &batch.schema, &row, positions)?
                    {
                        direct.insert(hash, index)?;
                    }
                } else if let Some(key) = self.key(
                    &self.left_keys,
                    self.left_key_positions.as_deref(),
                    &row,
                    &batch.schema,
                )? {
                    encoded_index
                        .as_mut()
                        .ok_or_else(|| ExecError::Other("join hash index is missing".into()))?
                        .insert(key, index)?;
                }
                left.push(row)?;
            }
        }
        self.right_input_spilled = SpillState::InMemory;

        let mut output = SpillBuffer::new(output_budget);
        if left.len() == 0 {
            self.output = Some(output.drain()?);
            return Ok(());
        }

        let direct_is_unique = direct_index.as_ref().is_some_and(|direct| {
            direct_positions.is_some_and(|(positions, _)| {
                !left.has_spilled() && direct.keys_are_unique(&left, &left.schema, positions)
            })
        });
        if direct_is_unique {
            self.right.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: left,
                hash_index: UniqueHashIndex::Direct(
                    direct_index
                        .take()
                        .ok_or_else(|| ExecError::Other("direct join index is missing".into()))?,
                ),
                build_left: true,
            });
            return Ok(());
        }

        let mut left_by_key = match encoded_index {
            Some(index) => index,
            None => {
                let (positions, _) = direct_positions
                    .ok_or_else(|| ExecError::Other("direct join positions are missing".into()))?;
                self.rebuild_encoded_index(&mut left, &self.left_keys, positions, hash_budget)?
            }
        };
        self.hash_index_spilled = left_by_key.has_spilled().into();
        if self.predicate.is_none() && !left.has_spilled() && left_by_key.is_memory_unique() {
            self.right.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: left,
                hash_index: UniqueHashIndex::Encoded(left_by_key),
                build_left: true,
            });
            return Ok(());
        }
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for right_row in batch.rows {
                let Some(key) = self.key(
                    &self.right_keys,
                    self.right_key_positions.as_deref(),
                    &right_row,
                    &batch.schema,
                )?
                else {
                    continue;
                };
                if self.predicate.is_none() {
                    match left_by_key.memory_match_summary(&key) {
                        Some(MemoryMatchSummary::Absent) => continue,
                        Some(MemoryMatchSummary::Single(index)) => {
                            let merged = left.with_row(index, |left_row| {
                                Ok(PhysicalRow::concat_right_owned(left_row, right_row))
                            })?;
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                            continue;
                        }
                        Some(MemoryMatchSummary::Multiple) | None => {}
                    }
                }
                left_by_key.for_each_match(&key, &mut |index| {
                    left.with_row(index, |left_row| {
                        let merged = PhysicalRow::concat(left_row, &right_row);
                        if self.matches(&merged)? {
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                        }
                        Ok(())
                    })
                })?;
            }
        }
        if !pending.is_empty() {
            output.push(Batch::from_physical_rows(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled().into();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn key(
        &self,
        expressions: &[ScalarExpr],
        positions: Option<&[usize]>,
        row: &PhysicalRow,
        schema: &RowSchema,
    ) -> ExecResult<Option<EncodedKey>> {
        if let Some(positions) = positions {
            let view = schema.view(row);
            return encode_non_null_key(positions.iter().map(|position| view.value_at(*position)));
        }
        let mut values = SmallVec::<[Value; 4]>::with_capacity(expressions.len());
        for expression in expressions {
            let value = self.evaluator.evaluate_physical(expression, schema, row)?;
            if matches!(value, Value::Null) {
                return Ok(None);
            }
            values.push(value);
        }
        encode_non_null_key(values.iter().map(Some))
    }

    fn matches(&self, row: &PhysicalRow) -> ExecResult<bool> {
        if let Some(predicate) = self.prepared_predicate.as_ref() {
            return Ok(predicate.keep_row(&self.schema.view(row))?);
        }
        self.predicate.as_ref().map_or(Ok(true), |predicate| {
            Ok(truthy(&self.evaluator.evaluate_physical(
                predicate,
                &self.schema,
                row,
            )?))
        })
    }

    fn next_streaming_unique(
        &mut self,
        state: &mut UniqueHashJoinState,
    ) -> ExecResult<Option<Batch>> {
        loop {
            let next = if state.build_left {
                self.right.next()?
            } else {
                self.left.next()?
            };
            let Some(batch) = next else {
                return Ok(None);
            };
            let mut output = Vec::with_capacity(batch.rows.len());
            for probe_row in batch.rows {
                let index = match &state.hash_index {
                    UniqueHashIndex::Direct(index) => {
                        let (build_positions, probe_positions) = if state.build_left {
                            (
                                self.left_key_positions.as_deref(),
                                self.right_key_positions.as_deref(),
                            )
                        } else {
                            (
                                self.right_key_positions.as_deref(),
                                self.left_key_positions.as_deref(),
                            )
                        };
                        let (Some(build_positions), Some(probe_positions)) =
                            (build_positions, probe_positions)
                        else {
                            return Err(ExecError::Other(
                                "direct join key positions are missing".into(),
                            ));
                        };
                        direct_unique_match(
                            index,
                            &state.build_rows,
                            build_positions,
                            &batch.schema,
                            &probe_row,
                            probe_positions,
                        )?
                    }
                    UniqueHashIndex::Encoded(index) => {
                        let expressions = if state.build_left {
                            &self.right_keys
                        } else {
                            &self.left_keys
                        };
                        let positions = if state.build_left {
                            self.right_key_positions.as_deref()
                        } else {
                            self.left_key_positions.as_deref()
                        };
                        let Some(key) =
                            self.key(expressions, positions, &probe_row, &batch.schema)?
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

impl PhysicalOperator for HashJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.estimated_cardinality
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.streaming_unique = None;
        self.output_spilled = SpillState::InMemory;
        self.right_input_spilled = SpillState::InMemory;
        self.hash_index_spilled = SpillState::InMemory;

        let state_budget = self.work_mem_bytes / 2;
        let output_budget = self.work_mem_bytes.saturating_sub(state_budget);
        if self.build_left {
            return self.open_build_left(state_budget, output_budget);
        }
        let right_budget = state_budget / 2;
        let hash_budget = state_budget.saturating_sub(right_budget);
        let right_schema = self.right.row_schema().clone();
        let mut right = HybridRowStore::new(right_schema, right_budget);
        let direct_positions = (matches!(self.kind, JoinKind::Inner) && self.predicate.is_none())
            .then_some(())
            .and(self.right_key_positions.as_deref())
            .zip(self.left_key_positions.as_deref());
        let mut direct_index = direct_positions.map(|_| DirectHashIndex::new(hash_budget));
        let mut encoded_index = direct_index
            .is_none()
            .then(|| HybridHashIndex::new(hash_budget));
        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for row in batch.rows {
                let index = right.len();
                if let (Some(direct), Some((positions, _))) =
                    (direct_index.as_mut(), direct_positions)
                {
                    if let Some(hash) =
                        positional_key_hash(direct.hasher(), &batch.schema, &row, positions)?
                    {
                        direct.insert(hash, index)?;
                    }
                } else if let Some(key) = self.key(
                    &self.right_keys,
                    self.right_key_positions.as_deref(),
                    &row,
                    &batch.schema,
                )? {
                    encoded_index
                        .as_mut()
                        .ok_or_else(|| ExecError::Other("join hash index is missing".into()))?
                        .insert(key, index)?;
                }
                right.push(row)?;
            }
        }
        self.right_input_spilled = right.has_spilled().into();

        if right.len() == 0 && matches!(self.kind, JoinKind::Inner) {
            let mut output = SpillBuffer::new(output_budget);
            self.output = Some(output.drain()?);
            return Ok(());
        }

        let direct_is_unique = direct_index.as_ref().is_some_and(|direct| {
            direct_positions.is_some_and(|(positions, _)| {
                !right.has_spilled() && direct.keys_are_unique(&right, &right.schema, positions)
            })
        });
        if direct_is_unique {
            self.left.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: right,
                hash_index: UniqueHashIndex::Direct(
                    direct_index
                        .take()
                        .ok_or_else(|| ExecError::Other("direct join index is missing".into()))?,
                ),
                build_left: false,
            });
            return Ok(());
        }

        let mut right_by_key = match encoded_index {
            Some(index) => index,
            None => {
                let (positions, _) = direct_positions
                    .ok_or_else(|| ExecError::Other("direct join positions are missing".into()))?;
                self.rebuild_encoded_index(&mut right, &self.right_keys, positions, hash_budget)?
            }
        };
        self.hash_index_spilled = right_by_key.has_spilled().into();
        if matches!(self.kind, JoinKind::Inner)
            && self.predicate.is_none()
            && !right.has_spilled()
            && right_by_key.is_memory_unique()
        {
            self.left.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: right,
                hash_index: UniqueHashIndex::Encoded(right_by_key),
                build_left: false,
            });
            return Ok(());
        }

        let mut matched_right = matches!(self.kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(right.len()))
            .transpose()?;
        let mut output = SpillBuffer::new(output_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for left_row in batch.rows {
                let mut matched_left = false;
                if let Some(key) = self.key(
                    &self.left_keys,
                    self.left_key_positions.as_deref(),
                    &left_row,
                    &batch.schema,
                )? {
                    if self.predicate.is_none() {
                        match right_by_key.memory_match_summary(&key) {
                            Some(MemoryMatchSummary::Absent) => {
                                if matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                                    push_output_row(
                                        &mut output,
                                        &mut pending,
                                        &self.schema,
                                        PhysicalRow::concat_left_owned(left_row, &self.right_nulls),
                                    )?;
                                }
                                continue;
                            }
                            Some(MemoryMatchSummary::Single(index)) => {
                                let merged = right.with_row(index, |right_row| {
                                    Ok(PhysicalRow::concat_left_owned(left_row, right_row))
                                })?;
                                push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                                if let Some(flags) = matched_right.as_mut() {
                                    flags.mark(index)?;
                                }
                                continue;
                            }
                            Some(MemoryMatchSummary::Multiple) | None => {}
                        }
                    }
                    right_by_key.for_each_match(&key, &mut |index| {
                        right.with_row(index, |right_row| {
                            let merged = PhysicalRow::concat(&left_row, right_row);
                            if self.matches(&merged)? {
                                push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                                if let Some(flags) = matched_right.as_mut() {
                                    flags.mark(index)?;
                                }
                                matched_left = true;
                            }
                            Ok(())
                        })
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
                ExecError::Other("right/full hash join has no match flags".into())
            })?;
            for index in 0..right.len() {
                if !matched_right.is_marked(index)? {
                    right.with_row(index, |right_row| {
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
        self.output_spilled = output.has_spilled().into();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if let Some(mut state) = self.streaming_unique.take() {
            let result = self.next_streaming_unique(&mut state);
            self.streaming_unique = Some(state);
            return result;
        }
        self.output
            .as_mut()
            .map_or(Ok(None), |output| output.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.streaming_unique = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right hash-join input")
    }
}

#[cfg(test)]
mod tests;
