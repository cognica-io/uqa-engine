//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The frame of one window function as the current row advances, tracked as `nodeWindowAgg.c` tracks it: a frame head and a frame tail that only move forward, the peer groups they pass, and the exclusion of the current row or its peers.

use super::partition::PartitionRows;
use uqa_core::Value;
use uqa_sql::ast::{FrameExclusion, FrameMode};
use uqa_sql::SQLError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundKind {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding,
    Following,
}

/// A frame definition with its offsets evaluated: `bigint` counts for `ROWS` and `GROUPS`, and values of the `in_range` offset type for `RANGE`.
#[derive(Debug, Clone)]
pub(super) struct FrameSpec {
    pub(super) mode: FrameMode,
    pub(super) start: BoundKind,
    pub(super) end: BoundKind,
    pub(super) exclusion: FrameExclusion,
    pub(super) start_offset: Option<Value>,
    pub(super) end_offset: Option<Value>,
    /// Direction and NULL placement of the ordering column a `RANGE` offset measures.
    pub(super) ascending: bool,
    pub(super) nulls_first: bool,
}

/// Where a row lies relative to the frame of the current row, as `row_is_in_frame` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Membership {
    /// Before the frame head, or excluded from the frame.
    Outside,
    Inside,
    /// Past the frame end; no later row is in the frame.
    After,
}

/// The current row and the peer group it belongs to, shared by every function of a window pass.
pub(super) struct CurrentRow {
    pub(super) position: i64,
    /// The number of the current row's peer group within the partition.
    pub(super) group: i64,
    /// The first row of the current row's peer group.
    pub(super) group_head: i64,
    group_tail: i64,
    group_tail_valid: bool,
}

impl CurrentRow {
    pub(super) const fn new() -> Self {
        Self {
            position: 0,
            group: 0,
            group_head: 0,
            group_tail: -1,
            group_tail_valid: false,
        }
    }

    /// Move to the next row, entering a new peer group when it is not a peer of the previous row.
    pub(super) fn advance(&mut self, rows: &mut PartitionRows<'_>) -> Result<(), SQLError> {
        self.position += 1;
        if !rows.are_peers(self.position - 1, self.position)? {
            self.group += 1;
            self.group_head = self.position;
            self.group_tail_valid = false;
        }
        Ok(())
    }

    /// `update_grouptailpos`: the first row after the current row's peer group.
    pub(super) fn group_tail(&mut self, rows: &mut PartitionRows<'_>) -> Result<i64, SQLError> {
        if self.group_tail_valid {
            return Ok(self.group_tail);
        }
        if rows.is_ordered() {
            loop {
                self.group_tail += 1;
                if self.group_tail >= rows.len() {
                    break;
                }
                if self.group_tail > self.position
                    && !rows.are_peers(self.group_tail, self.position)?
                {
                    break;
                }
            }
        } else {
            self.group_tail = rows.len();
        }
        self.group_tail_valid = true;
        Ok(self.group_tail)
    }
}

/// The frame head and tail of one window function. Both persist across rows of a partition and advance from where they were.
pub(super) struct FrameCursor {
    head: i64,
    head_group: i64,
    head_valid: bool,
    /// The first row after the frame.
    tail: i64,
    tail_group: i64,
    tail_valid: bool,
}

impl FrameCursor {
    pub(super) const fn new() -> Self {
        Self {
            head: 0,
            head_group: 0,
            head_valid: false,
            tail: 0,
            tail_group: 0,
            tail_valid: false,
        }
    }

    /// Forget the frame bounds of the previous current row.
    pub(super) fn invalidate(&mut self) {
        self.head_valid = false;
        self.tail_valid = false;
    }

    /// `update_frameheadpos`: the first row of the frame, before any exclusion.
    pub(super) fn head(
        &mut self,
        frame: &FrameSpec,
        current: &CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<i64, SQLError> {
        if self.head_valid {
            return Ok(self.head);
        }
        match frame.start {
            BoundKind::UnboundedPreceding => self.head = 0,
            BoundKind::CurrentRow => match frame.mode {
                FrameMode::Rows => self.head = current.position,
                FrameMode::Range | FrameMode::Groups => {
                    if rows.is_ordered() {
                        while self.head < rows.len()
                            && !rows.are_peers(self.head, current.position)?
                        {
                            self.head += 1;
                        }
                    } else {
                        self.head = 0;
                    }
                }
            },
            BoundKind::Preceding | BoundKind::Following => {
                let preceding = frame.start == BoundKind::Preceding;
                let offset = frame.start_offset.as_ref().ok_or_else(missing_offset)?;
                match frame.mode {
                    FrameMode::Rows => {
                        let head = shifted(current.position, count(offset)?, preceding);
                        self.head = head.clamp(0, rows.len());
                    }
                    FrameMode::Range => {
                        self.advance_range_head(frame, offset, preceding, current, rows)?;
                    }
                    FrameMode::Groups => {
                        let first_group = shifted(current.group, count(offset)?, preceding);
                        while self.head < rows.len() && self.head_group < first_group {
                            self.head += 1;
                            if self.head < rows.len()
                                && !rows.are_peers(self.head - 1, self.head)?
                            {
                                self.head_group += 1;
                            }
                        }
                    }
                }
            }
            BoundKind::UnboundedFollowing => {
                return Err(SQLError::Internal(
                    "window frame cannot start at UNBOUNDED FOLLOWING".into(),
                ))
            }
        }
        self.head_valid = true;
        Ok(self.head)
    }

    /// The first row whose ordering value `in_range` places at or after the current row moved by the offset. A NULL ordering value only neighbors other NULLs, which sort at one end.
    fn advance_range_head(
        &mut self,
        frame: &FrameSpec,
        offset: &Value,
        preceding: bool,
        current: &CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<(), SQLError> {
        let (sub, less) = if frame.ascending {
            (preceding, false)
        } else {
            (!preceding, true)
        };
        let current_value = rows.sort_value(current.position)?;
        while self.head < rows.len() {
            let head_value = rows.sort_value(self.head)?;
            let head_null = matches!(head_value, Value::Null);
            let current_null = matches!(current_value, Value::Null);
            if head_null || current_null {
                let stop = if frame.nulls_first {
                    !head_null || current_null
                } else {
                    head_null || !current_null
                };
                if stop {
                    break;
                }
            } else if uqa_sql::expr::in_range(&head_value, &current_value, offset, sub, less)? {
                break;
            }
            self.head += 1;
        }
        Ok(())
    }

    /// `update_frametailpos`: the first row after the frame, before any exclusion.
    pub(super) fn tail(
        &mut self,
        frame: &FrameSpec,
        current: &CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<i64, SQLError> {
        if self.tail_valid {
            return Ok(self.tail);
        }
        match frame.end {
            BoundKind::UnboundedFollowing => self.tail = rows.len(),
            BoundKind::CurrentRow => match frame.mode {
                FrameMode::Rows => self.tail = current.position + 1,
                FrameMode::Range | FrameMode::Groups => {
                    if rows.is_ordered() {
                        while self.tail < rows.len()
                            && (self.tail <= current.position
                                || rows.are_peers(self.tail, current.position)?)
                        {
                            self.tail += 1;
                        }
                    } else {
                        self.tail = rows.len();
                    }
                }
            },
            BoundKind::Preceding | BoundKind::Following => {
                let preceding = frame.end == BoundKind::Preceding;
                let offset = frame.end_offset.as_ref().ok_or_else(missing_offset)?;
                match frame.mode {
                    FrameMode::Rows => {
                        let end = shifted(current.position, count(offset)?, preceding);
                        self.tail = end.saturating_add(1).clamp(0, rows.len());
                    }
                    FrameMode::Range => {
                        self.advance_range_tail(frame, offset, preceding, current, rows)?;
                    }
                    FrameMode::Groups => {
                        let last_group = shifted(current.group, count(offset)?, preceding);
                        while self.tail < rows.len() && self.tail_group <= last_group {
                            self.tail += 1;
                            if self.tail < rows.len()
                                && !rows.are_peers(self.tail - 1, self.tail)?
                            {
                                self.tail_group += 1;
                            }
                        }
                    }
                }
            }
            BoundKind::UnboundedPreceding => {
                return Err(SQLError::Internal(
                    "window frame cannot end at UNBOUNDED PRECEDING".into(),
                ))
            }
        }
        self.tail_valid = true;
        Ok(self.tail)
    }

    fn advance_range_tail(
        &mut self,
        frame: &FrameSpec,
        offset: &Value,
        preceding: bool,
        current: &CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<(), SQLError> {
        let (sub, less) = if frame.ascending {
            (preceding, true)
        } else {
            (!preceding, false)
        };
        let current_value = rows.sort_value(current.position)?;
        while self.tail < rows.len() {
            let tail_value = rows.sort_value(self.tail)?;
            let tail_null = matches!(tail_value, Value::Null);
            let current_null = matches!(current_value, Value::Null);
            if tail_null || current_null {
                let stop = if frame.nulls_first {
                    !tail_null
                } else {
                    !current_null
                };
                if stop {
                    break;
                }
            } else if !uqa_sql::expr::in_range(&tail_value, &current_value, offset, sub, less)? {
                break;
            }
            self.tail += 1;
        }
        Ok(())
    }

    /// `row_is_in_frame`.
    pub(super) fn membership(
        &mut self,
        frame: &FrameSpec,
        current: &mut CurrentRow,
        rows: &mut PartitionRows<'_>,
        position: i64,
    ) -> Result<Membership, SQLError> {
        if position < self.head(frame, current, rows)? {
            return Ok(Membership::Outside);
        }
        match (frame.end, frame.mode) {
            (BoundKind::CurrentRow, FrameMode::Rows) => {
                if position > current.position {
                    return Ok(Membership::After);
                }
            }
            (BoundKind::CurrentRow, FrameMode::Range | FrameMode::Groups) => {
                if position > current.position && !rows.are_peers(position, current.position)? {
                    return Ok(Membership::After);
                }
            }
            (BoundKind::Preceding | BoundKind::Following, FrameMode::Rows) => {
                let offset = count(frame.end_offset.as_ref().ok_or_else(missing_offset)?)?;
                let offset = if frame.end == BoundKind::Preceding {
                    -offset
                } else {
                    offset
                };
                // An end beyond bigint lies past the partition.
                if current
                    .position
                    .checked_add(offset)
                    .is_some_and(|end| position > end)
                {
                    return Ok(Membership::After);
                }
            }
            (BoundKind::Preceding | BoundKind::Following, FrameMode::Range | FrameMode::Groups) => {
                if position >= self.tail(frame, current, rows)? {
                    return Ok(Membership::After);
                }
            }
            (BoundKind::UnboundedFollowing | BoundKind::UnboundedPreceding, _) => {}
        }
        let excluded = match frame.exclusion {
            FrameExclusion::NoOthers => false,
            FrameExclusion::CurrentRow => position == current.position,
            FrameExclusion::Group => in_current_group(current, rows, position)?,
            FrameExclusion::Ties => {
                position != current.position && in_current_group(current, rows, position)?
            }
        };
        Ok(if excluded {
            Membership::Outside
        } else {
            Membership::Inside
        })
    }

    /// `WinGetFuncArgInFrame`: the row `offset` rows after the frame head, or `offset` rows (zero or negative) from the frame's last row, counting only the rows the exclusion leaves.
    pub(super) fn seek(
        &mut self,
        frame: &FrameSpec,
        current: &mut CurrentRow,
        rows: &mut PartitionRows<'_>,
        from_head: bool,
        offset: i64,
    ) -> Result<Option<i64>, SQLError> {
        let position = if from_head {
            if offset < 0 {
                return Ok(None);
            }
            let head = self.head(frame, current, rows)?;
            let mut position = head.saturating_add(offset);
            match frame.exclusion {
                FrameExclusion::NoOthers => {}
                FrameExclusion::CurrentRow => {
                    if position >= current.position && current.position >= head {
                        position += 1;
                    }
                }
                FrameExclusion::Group | FrameExclusion::Ties => {
                    let group_tail = current.group_tail(rows)?;
                    if position >= current.group_head && group_tail > head {
                        let overlap_start = current.group_head.max(head);
                        if frame.exclusion == FrameExclusion::Ties {
                            if position == overlap_start {
                                position = current.position;
                            } else {
                                position += group_tail - overlap_start - 1;
                            }
                        } else {
                            position += group_tail - overlap_start;
                        }
                    }
                }
            }
            position
        } else {
            if offset > 0 {
                return Ok(None);
            }
            let tail = self.tail(frame, current, rows)?;
            let mut position = tail - 1 + offset;
            match frame.exclusion {
                FrameExclusion::NoOthers => {}
                FrameExclusion::CurrentRow => {
                    if position <= current.position && current.position < tail {
                        position -= 1;
                    }
                }
                FrameExclusion::Group | FrameExclusion::Ties => {
                    let group_tail = current.group_tail(rows)?;
                    if position < group_tail && current.group_head < tail {
                        let overlap_end = group_tail.min(tail);
                        if frame.exclusion == FrameExclusion::Ties {
                            if position == overlap_end - 1 {
                                position = current.position;
                            } else {
                                position -= overlap_end - 1 - current.group_head;
                            }
                        } else {
                            position -= overlap_end - current.group_head;
                        }
                    }
                }
            }
            if frame.exclusion != FrameExclusion::NoOthers
                && position < self.head(frame, current, rows)?
            {
                return Ok(None);
            }
            position
        };
        if position < 0 || position >= rows.len() {
            return Ok(None);
        }
        Ok(
            (self.membership(frame, current, rows, position)? == Membership::Inside)
                .then_some(position),
        )
    }
}

/// Whether a row belongs to the current row's peer group; without `ORDER BY` every row does.
fn in_current_group(
    current: &mut CurrentRow,
    rows: &mut PartitionRows<'_>,
    position: i64,
) -> Result<bool, SQLError> {
    if !rows.is_ordered() {
        return Ok(true);
    }
    Ok(position >= current.group_head && position < current.group_tail(rows)?)
}

/// `base` moved back or forward by a non-negative count; a move past bigint lies beyond every partition.
fn shifted(base: i64, count: i64, back: bool) -> i64 {
    if back {
        base - count
    } else {
        base.checked_add(count).unwrap_or(i64::MAX)
    }
}

fn count(offset: &Value) -> Result<i64, SQLError> {
    match offset {
        Value::Int(count) => Ok(*count),
        other => Err(SQLError::Internal(format!(
            "ROWS or GROUPS frame offset {other:?} is not a bigint"
        ))),
    }
}

fn missing_offset() -> SQLError {
    SQLError::Internal("window frame offset was not evaluated".into())
}
