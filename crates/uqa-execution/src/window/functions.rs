//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One window function call evaluated row by row over a sorted partition, as `windowfuncs.c` and `eval_windowaggregates` evaluate them: ranking and distribution over peers, `ntile` buckets, `lag` and `lead` within the partition, `first_value`, `last_value` and `nth_value` within the frame, and aggregates over the frame.

use super::aggregates::WindowAggregate;
use super::frame::{CurrentRow, FrameCursor, FrameSpec};
use super::partition::PartitionRows;
use uqa_core::Value;
use uqa_sql::SQLError;

type PreparedExpression = crate::scalar::PreparedExpressions<uqa_sql::ScalarExpr>;

pub(super) enum WindowFunction {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile(PreparedExpression),
    Shift(Box<Shift>),
    FirstValue(PreparedExpression),
    LastValue(PreparedExpression),
    NthValue {
        target: PreparedExpression,
        position: PreparedExpression,
    },
    Aggregate(Box<WindowAggregate>),
}

/// `lag` (`forward` false) or `lead`: the target expression on the row `offset` rows away within the partition, or `default` on the current row when that row does not exist.
pub(super) struct Shift {
    pub(super) forward: bool,
    pub(super) target: PreparedExpression,
    pub(super) offset: Option<PreparedExpression>,
    pub(super) default: Option<PreparedExpression>,
}

/// `ntile`'s partition state: the current bucket, the rows it holds so far, the row count that closes it, and how many leading buckets take one extra row.
#[derive(Default)]
struct NtileState {
    bucket: i64,
    rows_in_bucket: i64,
    boundary: i64,
    remainder: i64,
}

pub(super) struct WindowFunctionState {
    function: WindowFunction,
    frame: FrameSpec,
    cursor: FrameCursor,
    /// `rank_context`: the rank (or dense rank, or peer-inclusive row count for `cume_dist`) of the current row; zero before the partition's first row.
    rank: i64,
    ntile: NtileState,
}

impl WindowFunctionState {
    pub(super) fn new(function: WindowFunction, frame: FrameSpec) -> Self {
        Self {
            function,
            frame,
            cursor: FrameCursor::new(),
            rank: 0,
            ntile: NtileState::default(),
        }
    }

    pub(super) fn frame_mut(&mut self) -> &mut FrameSpec {
        &mut self.frame
    }

    pub(super) fn begin_partition(&mut self) {
        self.cursor = FrameCursor::new();
        self.rank = 0;
        self.ntile = NtileState::default();
        if let WindowFunction::Aggregate(aggregate) = &mut self.function {
            aggregate.begin_partition();
        }
    }

    /// Forget the frame bounds of the previous row before the current row moves.
    pub(super) fn advance(&mut self) {
        self.cursor.invalidate();
    }

    pub(super) fn value(
        &mut self,
        current: &mut CurrentRow,
        rows: &mut PartitionRows<'_>,
    ) -> Result<Value, SQLError> {
        let position = current.position;
        match &mut self.function {
            WindowFunction::RowNumber => Ok(Value::Int(position + 1)),
            WindowFunction::Rank => {
                if rank_up(&mut self.rank, current, rows)? {
                    self.rank = position + 1;
                }
                Ok(Value::Int(self.rank))
            }
            WindowFunction::DenseRank => {
                if rank_up(&mut self.rank, current, rows)? {
                    self.rank += 1;
                }
                Ok(Value::Int(self.rank))
            }
            WindowFunction::PercentRank => {
                if rank_up(&mut self.rank, current, rows)? {
                    self.rank = position + 1;
                }
                let total = rows.len();
                Ok(Value::Float(if total <= 1 {
                    0.0
                } else {
                    (self.rank - 1) as f64 / (total - 1) as f64
                }))
            }
            WindowFunction::CumeDist => {
                let up = rank_up(&mut self.rank, current, rows)?;
                if up || self.rank == 1 {
                    // Count the rows up to the end of the current row's peer group.
                    self.rank = position + 1;
                    let mut row = self.rank;
                    while row < rows.len() && rows.are_peers(row - 1, row)? {
                        self.rank += 1;
                        row += 1;
                    }
                }
                Ok(Value::Float(self.rank as f64 / rows.len() as f64))
            }
            WindowFunction::Ntile(argument) => ntile(&mut self.ntile, argument, current, rows),
            WindowFunction::Shift(shift) => shift_value(shift, current, rows),
            WindowFunction::FirstValue(target) => frame_value(
                &self.frame,
                &mut self.cursor,
                target,
                (true, 0),
                current,
                rows,
            ),
            WindowFunction::LastValue(target) => frame_value(
                &self.frame,
                &mut self.cursor,
                target,
                (false, 0),
                current,
                rows,
            ),
            WindowFunction::NthValue {
                target,
                position: nth,
            } => {
                let nth = match rows.evaluate(nth, position)? {
                    Value::Null => return Ok(Value::Null),
                    Value::Int(nth) => nth,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "nth_value position {other:?} is not an integer"
                        )))
                    }
                };
                if nth <= 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "22016".into(),
                        message: "argument of nth_value must be greater than zero".into(),
                    });
                }
                frame_value(
                    &self.frame,
                    &mut self.cursor,
                    target,
                    (true, nth - 1),
                    current,
                    rows,
                )
            }
            WindowFunction::Aggregate(aggregate) => {
                aggregate.value(&self.frame, &mut self.cursor, current, rows)
            }
        }
    }
}

/// `leadlag_common`: an offset evaluated on the current row, NULL giving NULL; a target row outside the partition gives the default evaluated on the current row.
fn shift_value(
    shift: &Shift,
    current: &CurrentRow,
    rows: &mut PartitionRows<'_>,
) -> Result<Value, SQLError> {
    let position = current.position;
    let offset = match &shift.offset {
        Some(offset) => match rows.evaluate(offset, position)? {
            Value::Null => return Ok(Value::Null),
            Value::Int(offset) => offset,
            other => {
                return Err(SQLError::Internal(format!(
                    "window offset {other:?} is not an integer"
                )))
            }
        },
        None => 1,
    };
    let target_position = if shift.forward {
        position.checked_add(offset)
    } else {
        position.checked_sub(offset)
    };
    match target_position.filter(|target| (0..rows.len()).contains(target)) {
        Some(target_position) => rows.evaluate(&shift.target, target_position),
        None => shift
            .default
            .as_ref()
            .map_or(Ok(Value::Null), |default| rows.evaluate(default, position)),
    }
}

/// `WinGetFuncArgInFrame`: the target expression on the row `offset` rows from the frame head (`from_head`) or from its last row, NULL when that row is not in the frame.
fn frame_value(
    frame: &FrameSpec,
    cursor: &mut FrameCursor,
    target: &PreparedExpression,
    (from_head, offset): (bool, i64),
    current: &mut CurrentRow,
    rows: &mut PartitionRows<'_>,
) -> Result<Value, SQLError> {
    cursor
        .seek(frame, current, rows, from_head, offset)?
        .map_or(Ok(Value::Null), |found| rows.evaluate(target, found))
}

/// `rank_up`: whether the current row starts a new peer group. The partition's first row starts the rank at one without counting as a step up.
fn rank_up(
    rank: &mut i64,
    current: &CurrentRow,
    rows: &mut PartitionRows<'_>,
) -> Result<bool, SQLError> {
    if *rank == 0 {
        *rank = 1;
        return Ok(false);
    }
    Ok(!rows.are_peers(current.position - 1, current.position)?)
}

/// `window_ntile`: the bucket count is read from the first row whose argument is not NULL; until then every row's result is NULL.
fn ntile(
    state: &mut NtileState,
    argument: &PreparedExpression,
    current: &CurrentRow,
    rows: &mut PartitionRows<'_>,
) -> Result<Value, SQLError> {
    if state.bucket == 0 {
        let buckets = match rows.evaluate(argument, current.position)? {
            Value::Null => return Ok(Value::Null),
            Value::Int(buckets) => buckets,
            other => {
                return Err(SQLError::Internal(format!(
                    "ntile bucket count {other:?} is not an integer"
                )))
            }
        };
        if buckets <= 0 {
            return Err(SQLError::Routine {
                sqlstate: "22014".into(),
                message: "argument of ntile must be greater than zero".into(),
            });
        }
        let total = rows.len();
        state.bucket = 1;
        state.rows_in_bucket = 0;
        state.boundary = total / buckets;
        if state.boundary <= 0 {
            state.boundary = 1;
        } else {
            state.remainder = total % buckets;
            if state.remainder != 0 {
                state.boundary += 1;
            }
        }
    }
    state.rows_in_bucket += 1;
    if state.boundary < state.rows_in_bucket {
        if state.remainder != 0 && state.bucket == state.remainder {
            state.remainder = 0;
            state.boundary -= 1;
        }
        state.bucket += 1;
        state.rows_in_bucket = 1;
    }
    Ok(Value::Int(state.bucket))
}
