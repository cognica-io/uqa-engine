//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Classify MERGE inputs in target order and emit unmatched sources in their original order.

mod candidates;
use candidates::MergeCandidates;

use super::codec::MergePairKind;
use crate::mutation::{
    expressions::MutationPredicate, rows::join_rows, statement::context::MutationStatementContext,
};
use crate::physical::physical_exec_error;
use crate::query::CteScope;
use crate::spill::{MatchFlags, SpillBatchWriter};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema, SharedSpill, SpillBuffer};
use uqa_sql::{
    plan::{MergePlan, MergeWhenPlan},
    SQLError, SQLParam,
};

pub(super) struct MergeInputPairs<'a, S: Clone + 'static> {
    source: &'a SharedSpill,
    candidates: MergeCandidates<'a>,
    visible: MutationPredicate<'a, S>,
    condition: MutationPredicate<'a, S>,
    matched: Option<MatchFlags>,
    source_missing: bool,
    null_source: OwnedPhysicalRow,
    writer: SpillBatchWriter,
    output: SpillBuffer,
    schema: RowSchema,
}

impl<'a, S: Clone + 'static> MergeInputPairs<'a, S> {
    pub(super) fn new(
        context: &MutationStatementContext<'a, S>,
        plan: &'a MergePlan,
        params: &'a [SQLParam],
        ctes: &'a CteScope<S>,
        source: &'a SharedSpill,
        schema: RowSchema,
        work_mem: usize,
    ) -> Result<Self, SQLError> {
        let expressions = context
            .mutation
            .preparation
            .referential
            .assignment
            .expressions;
        let candidates = MergeCandidates::new(
            &plan.join_condition,
            params,
            expressions.types,
            context.query.source.volatility,
            work_mem,
            context.query.source.ctes.production_resources()?,
        );
        let source_missing = plan.when_clauses.iter().any(|clause| {
            matches!(
                clause,
                MergeWhenPlan::UpdateNotMatchedBySource { .. }
                    | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                    | MergeWhenPlan::NothingNotMatchedBySource { .. }
            )
        });
        Ok(Self {
            source,
            candidates,
            visible: MutationPredicate::new(
                expressions,
                ctes,
                plan.target_predicate.as_ref(),
                params,
            ),
            condition: MutationPredicate::new(
                expressions,
                ctes,
                Some(&plan.join_condition),
                params,
            ),
            matched: None,
            source_missing,
            null_source: OwnedPhysicalRow::new(
                source.row_schema().clone(),
                PhysicalRow::nulls(source.row_schema().physical_width()),
            ),
            writer: SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?,
            output: SpillBuffer::new(work_mem),
            schema,
        })
    }

    pub(super) fn pair_target(
        &mut self,
        target: &OwnedPhysicalRow,
        mut encode: impl FnMut(MergePairKind, &OwnedPhysicalRow) -> Result<PhysicalRow, SQLError>,
    ) -> Result<bool, SQLError> {
        if !self.visible.qualifies(target)? {
            return Ok(false);
        }
        let mut found = false;
        self.candidates
            .visit(target, self.source, |ordinal, source| {
                if !self.condition.qualifies(&join_rows(target, source))? {
                    return Ok(());
                }
                let flags = match &mut self.matched {
                    Some(flags) => flags,
                    slot @ None => slot.insert(
                        MatchFlags::new(self.source.rows() as u64, "MERGE")
                            .map_err(physical_exec_error)?,
                    ),
                };
                flags.mark(ordinal).map_err(physical_exec_error)?;
                self.writer
                    .push(&mut self.output, encode(MergePairKind::Matched, source)?)
                    .map_err(physical_exec_error)?;
                found = true;
                Ok(())
            })?;
        if !found && self.source_missing {
            self.writer
                .push(
                    &mut self.output,
                    encode(MergePairKind::NotMatchedBySource, &self.null_source)?,
                )
                .map_err(physical_exec_error)?;
        }
        Ok(found)
    }

    pub(super) fn finish(
        mut self,
        mut encode_unmatched: impl FnMut(&OwnedPhysicalRow) -> PhysicalRow,
    ) -> Result<SharedSpill, SQLError> {
        for (ordinal, source) in self
            .source
            .read_rows()
            .map_err(physical_exec_error)?
            .enumerate()
        {
            uqa_sql::expr::check_statement_cancellation()?;
            let source = source.map_err(physical_exec_error)?;
            if let Some(matched) = &mut self.matched {
                if matched
                    .is_marked(ordinal as u64)
                    .map_err(physical_exec_error)?
                {
                    continue;
                }
            }
            self.writer
                .push(&mut self.output, encode_unmatched(&source))
                .map_err(physical_exec_error)?;
        }
        self.writer
            .finish(&mut self.output)
            .map_err(physical_exec_error)?;
        self.output
            .into_shared(self.schema)
            .map_err(physical_exec_error)
    }
}
