//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resume one probe and duplicate-key cursor without evaluating later candidates.

use super::canonical_index::HashMatches;
use super::{
    DirectHashIndex, DirectMatches, HashJoin, HybridHashIndex, HybridRowStore, JoinHashIndex,
    JoinKind, MatchFlags,
};
use crate::RowSchemaExecution;
use crate::{Batch, ExecError, ExecResult, PhysicalRow};

pub(super) struct HashJoinState {
    pub(super) build_rows: HybridRowStore,
    index: ProbeIndex,
    pub(super) build_left: bool,
    matched_build: Option<MatchFlags>,
    probe: Option<PhysicalRow>,
    matched_probe: bool,
    probe_finished: bool,
    unmatched_position: u64,
}

enum ProbeIndex {
    Direct(DirectHashIndex, DirectMatches),
    Encoded(HybridHashIndex, HashMatches),
}

impl HashJoinState {
    #[cfg(test)]
    pub(super) fn uses_direct_index(&self) -> bool {
        matches!(self.index, ProbeIndex::Direct(..))
    }

    pub(super) fn new(
        build_rows: HybridRowStore,
        index: JoinHashIndex,
        build_left: bool,
        kind: JoinKind,
    ) -> ExecResult<Self> {
        let matched_build = matches!(kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(build_rows.len(), "join"))
            .transpose()?;
        Ok(Self {
            build_rows,
            index: match index {
                JoinHashIndex::Direct(index) => ProbeIndex::Direct(index, DirectMatches::default()),
                JoinHashIndex::Encoded(index) => ProbeIndex::Encoded(index, HashMatches::empty()),
            },
            build_left,
            matched_build,
            probe: None,
            matched_probe: false,
            probe_finished: false,
            unmatched_position: 0,
        })
    }
}

impl HashJoin<'_> {
    fn next_probe_row(&mut self, state: &mut HashJoinState, demanded: bool) -> ExecResult<bool> {
        if !state.build_left {
            if let Some(batch) = self.prefetched_left.take() {
                self.probe_pending.retain_batch(batch);
            }
        }
        let input = if state.build_left {
            self.right.as_mut()
        } else {
            self.left.as_mut()
        };
        let Some(row) = self.probe_pending.next_row(input, demanded)? else {
            return Ok(false);
        };
        let schema = input.row_schema().clone();
        match &mut state.index {
            ProbeIndex::Direct(index, matches) => {
                *matches = DirectMatches::for_probe(
                    index,
                    schema.view(&row),
                    self.positional_keys(!state.build_left)?,
                    self.indeterminate_keys(),
                    self.evaluator.enum_labels(),
                )?;
            }
            ProbeIndex::Encoded(index, matches) => {
                *matches = match self.key_for_side(!state.build_left, &row, &schema)? {
                    Some(key) => index.match_cursor(key)?,
                    None => HashMatches::empty(),
                };
            }
        }
        state.probe = Some(row);
        state.matched_probe = false;
        Ok(true)
    }

    fn next_hash_row(
        &mut self,
        state: &mut HashJoinState,
        demanded: bool,
    ) -> ExecResult<Option<PhysicalRow>> {
        loop {
            uqa_sql::expr::check_statement_cancellation().map_err(uqa_sql::SQLError::from)?;
            if state.probe.is_none() && !state.probe_finished {
                state.probe_finished = !self.next_probe_row(state, demanded)?;
            }
            if state.probe_finished {
                let Some(flags) = state.matched_build.as_mut() else {
                    return Ok(None);
                };
                while state.unmatched_position < state.build_rows.len() {
                    let position = state.unmatched_position;
                    state.unmatched_position += 1;
                    if !flags.is_marked(position)? {
                        return state.build_rows.with_row(position, |row| {
                            Ok(Some(PhysicalRow::concat(&self.left_nulls, row)))
                        });
                    }
                }
                return Ok(None);
            }
            let probe = state
                .probe
                .as_ref()
                .ok_or_else(|| ExecError::Other("hash join candidate has no probe".into()))?;
            let next = match &mut state.index {
                ProbeIndex::Direct(index, matches) => {
                    let schema = if state.build_left {
                        self.right.row_schema()
                    } else {
                        self.left.row_schema()
                    };
                    matches.next(
                        index,
                        &state.build_rows,
                        self.positional_keys(state.build_left)?,
                        schema.view(probe),
                        self.positional_keys(!state.build_left)?,
                        self.evaluator.enum_labels(),
                    )?
                }
                ProbeIndex::Encoded(_, matches) => matches.next()?,
            };
            if let Some(position) = next {
                let joined = state.build_rows.with_row(position, |build| {
                    let joined = if state.build_left {
                        PhysicalRow::concat(build, probe)
                    } else {
                        PhysicalRow::concat(probe, build)
                    };
                    Ok(self.matches(&joined)?.then_some(joined))
                })?;
                if let Some(row) = joined {
                    state.matched_probe = true;
                    if let Some(flags) = state.matched_build.as_mut() {
                        flags.mark(position)?;
                    }
                    return Ok(Some(row));
                }
                continue;
            }
            let probe = state.probe.take().ok_or_else(|| {
                ExecError::Other("hash join exhausted candidates without a probe".into())
            })?;
            if !state.matched_probe && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                return Ok(Some(PhysicalRow::concat_left_owned(
                    probe,
                    &self.right_nulls,
                )));
            }
        }
    }

    pub(super) fn next_streaming(
        &mut self,
        state: &mut HashJoinState,
        rows: usize,
        demanded: bool,
    ) -> ExecResult<Option<Batch>> {
        let mut output = std::mem::take(&mut self.output);
        let result =
            output.drain_join_batch(self.schema.clone(), self.work_mem_bytes, rows, || {
                self.next_hash_row(state, demanded)
            });
        self.output = output;
        result
    }
}
