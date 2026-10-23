//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resume one probe and duplicate-key cursor without evaluating later candidates.

use super::canonical_index::HashMatches;
use super::{HashJoin, HybridHashIndex, HybridRowStore, JoinKind, MatchFlags, SpillState};
use crate::spill::SpillBatchWriter;
use crate::{Batch, ExecError, ExecResult, PhysicalRow};

pub(super) struct HashJoinState {
    pub(super) build_rows: HybridRowStore,
    pub(super) index: HybridHashIndex,
    pub(super) build_left: bool,
    matched_build: Option<MatchFlags>,
    probe: Option<PhysicalRow>,
    matches: HashMatches,
    matched_probe: bool,
    probe_finished: bool,
    unmatched_position: u64,
}

impl HashJoinState {
    pub(super) fn new(
        build_rows: HybridRowStore,
        index: HybridHashIndex,
        build_left: bool,
        kind: JoinKind,
    ) -> ExecResult<Self> {
        let matched_build = matches!(kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(build_rows.len(), "join"))
            .transpose()?;
        Ok(Self {
            build_rows,
            index,
            build_left,
            matched_build,
            probe: None,
            matches: HashMatches::empty(),
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
        let (keys, types, positions) = if state.build_left {
            (
                &self.right_keys,
                &self.right_key_types,
                self.right_key_positions.as_deref(),
            )
        } else {
            (
                &self.left_keys,
                &self.left_key_types,
                self.left_key_positions.as_deref(),
            )
        };
        state.matches = match self.key(keys, types, positions, &row, &schema)? {
            Some(key) => state.index.match_cursor(key)?,
            None => HashMatches::empty(),
        };
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
            if let Some(position) = state.matches.next()? {
                let probe = state
                    .probe
                    .as_ref()
                    .ok_or_else(|| ExecError::Other("hash join candidate has no probe".into()))?;
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
        if let Some(batch) = self.output.as_mut().and_then(Iterator::next).transpose()? {
            return Ok(Some(batch));
        }
        self.output = None;
        let mut output =
            crate::SpillBuffer::new(self.work_mem_bytes.saturating_sub(self.work_mem_bytes / 2));
        let mut writer = SpillBatchWriter::new(self.schema.clone())?;
        let mut produced = 0;
        while produced < rows {
            let Some(row) = self.next_hash_row(state, demanded)? else {
                break;
            };
            writer.push(&mut output, row)?;
            produced += 1;
        }
        if produced == 0 {
            return Ok(None);
        }
        writer.finish(&mut output)?;
        if output.has_spilled() {
            self.output_spilled = SpillState::Spilled;
        }
        self.output = Some(output.drain()?);
        self.output
            .as_mut()
            .expect("produced hash output")
            .next()
            .transpose()
    }
}
