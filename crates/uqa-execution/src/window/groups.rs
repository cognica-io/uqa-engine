//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GROUPS offsets traverse peer groups and retain out-of-partition sentinels.

use super::{
    eval_frame_offset, evaluate_order_key, exec_to_sql_error, BufferedIndexedSpill, SQLError,
    SQLParam, ScalarFrameBound, ScalarSubqueryRunner, ScalarWindowSpec,
};

#[expect(
    clippy::too_many_arguments,
    reason = "retains the selected frame evaluation context"
)]
pub(super) fn resolve(
    partition: &mut BufferedIndexedSpill,
    current: u64,
    spec: &ScalarWindowSpec,
    bound: &ScalarFrameBound,
    is_start: bool,
    params: &[SQLParam],
    hook: &dyn uqa_sql::expr::EngineHook,
    subqueries: &dyn ScalarSubqueryRunner,
) -> Result<i128, SQLError> {
    let schema = partition.row_schema().clone();
    let length = partition.len();
    let (offset, following) = match bound {
        ScalarFrameBound::UnboundedPreceding => return Ok(0),
        ScalarFrameBound::UnboundedFollowing => return Ok(i128::from(length) - 1),
        ScalarFrameBound::CurrentRow => (0, true),
        ScalarFrameBound::Following(expression) | ScalarFrameBound::Preceding(expression) => {
            let row = partition.get(current).map_err(exec_to_sql_error)?;
            (
                eval_frame_offset(expression, &schema, &row, params, hook, subqueries)?,
                matches!(bound, ScalarFrameBound::Following(_)),
            )
        }
    };
    let mut key_at = |position| {
        let row = partition.get(position).map_err(exec_to_sql_error)?;
        evaluate_order_key(&spec.order_by, &schema, &row, params, hook, subqueries)
    };
    let mut position = current;
    let mut key = key_at(position)?;
    for _ in 0..offset {
        loop {
            position = if following {
                if position + 1 == length {
                    return Ok(i128::from(length));
                }
                position + 1
            } else {
                let Some(previous) = position.checked_sub(1) else {
                    return Ok(-1);
                };
                previous
            };
            let next_key = key_at(position)?;
            if next_key != key {
                key = next_key;
                break;
            }
        }
    }
    loop {
        let next = if is_start {
            let Some(previous) = position.checked_sub(1) else {
                break;
            };
            previous
        } else {
            if position + 1 == length {
                break;
            }
            position + 1
        };
        if key_at(next)? != key {
            break;
        }
        position = next;
    }
    Ok(i128::from(position))
}
