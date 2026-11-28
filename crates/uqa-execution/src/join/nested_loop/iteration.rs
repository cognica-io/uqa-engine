//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Demand fills one repeatable right prefix and advances each join candidate once.

use super::{Batch, ExecError, ExecResult, JoinKind, NestedLoopJoin, PhysicalRow, RightInputState};

impl NestedLoopJoin<'_> {
    fn ensure_right(&mut self, position: u64, demanded: bool) -> ExecResult<bool> {
        loop {
            let rows = self
                .right_rows
                .as_ref()
                .ok_or_else(|| ExecError::Other("nested-loop join is not open".into()))?;
            if position < rows.len() {
                return Ok(true);
            }
            if self.right_state == RightInputState::Exhausted {
                return Ok(false);
            }
            if self.right_state == RightInputState::Unopened {
                self.right_state = RightInputState::Open;
                self.right.open()?;
            }
            let Some(row) = self.right_pending.next_row(self.right.as_mut(), demanded)? else {
                self.right_state = RightInputState::Exhausted;
                self.right.close()?;
                return Ok(false);
            };
            let rows = self.right_rows.as_mut().expect("open right store");
            rows.push(row)?;
            self.right_input_spilled = rows.has_spilled().into();
            if let Some(flags) = self.matched_right.as_mut() {
                flags.push_unmarked()?;
            }
        }
    }

    fn next_joined_row(&mut self, demanded: bool) -> ExecResult<Option<PhysicalRow>> {
        loop {
            uqa_sql::expr::check_statement_cancellation().map_err(uqa_sql::SQLError::from)?;
            if self.current_left.is_none() && !self.left_finished {
                let Some(row) = self.left_pending.next_row(self.left.as_mut(), demanded)? else {
                    self.left_finished = true;
                    continue;
                };
                self.current_left = Some(row);
                self.right_position = 0;
                self.matched_left = false;
            }
            if self.left_finished {
                if !matches!(self.kind, JoinKind::Right | JoinKind::Full)
                    || !self.ensure_right(self.unmatched_position, demanded)?
                {
                    return Ok(None);
                }
                let position = self.unmatched_position;
                self.unmatched_position += 1;
                let flags = self
                    .matched_right
                    .as_mut()
                    .expect("right-preserving match flags");
                if flags.is_marked(position)? {
                    continue;
                }
                let nulls = &self.left_nulls;
                return self
                    .right_rows
                    .as_mut()
                    .expect("open right store")
                    .with_row(position, |right| {
                        Ok(Some(PhysicalRow::concat(nulls, right)))
                    });
            }
            if !self.ensure_right(self.right_position, demanded)? {
                let left = self.current_left.take().expect("current left row");
                if !self.matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                    return Ok(Some(PhysicalRow::concat_left_owned(
                        left,
                        &self.right_nulls,
                    )));
                }
                continue;
            }
            let position = self.right_position;
            self.right_position += 1;
            let left = self.current_left.as_ref().expect("current left row");
            let predicate = &self.predicate;
            let evaluator = &self.evaluator;
            let schema = &self.schema;
            let joined = self
                .right_rows
                .as_mut()
                .expect("open right store")
                .with_row(position, |right| {
                    let joined = PhysicalRow::concat(left, right);
                    let matched = match predicate {
                        None => true,
                        Some(predicate) => uqa_sql::expr::truthy(
                            &evaluator.evaluate_physical_with_function_states(
                                predicate,
                                schema,
                                &joined,
                                predicate.calls(),
                            )?,
                        )?,
                    };
                    Ok(matched.then_some(joined))
                })?;
            if let Some(joined) = joined {
                self.matched_left = true;
                if let Some(flags) = self.matched_right.as_mut() {
                    flags.mark(position)?;
                }
                return Ok(Some(joined));
            }
        }
    }

    pub(super) fn pull(&mut self, rows: usize, demanded: bool) -> ExecResult<Option<Batch>> {
        let mut output = std::mem::take(&mut self.output);
        let result =
            output.drain_join_batch(self.schema.clone(), self.work_mem_bytes, rows, || {
                self.next_joined_row(demanded)
            });
        self.output = output;
        result
    }
}
