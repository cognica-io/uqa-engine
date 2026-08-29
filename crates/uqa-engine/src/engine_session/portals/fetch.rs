//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming, materialization, positioning, and row selection for portals.

use super::{
    CursorDirection, Engine, SQLError, SQLResult, SessionPortalData, SessionPortalMaterialization,
    SessionPortalPosition, SessionPortalState, Value,
};

fn start_streaming_portal(engine: &Engine, state: &mut SessionPortalState) {
    let pending = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    let SessionPortalData::Pending {
        query,
        params,
        table_snapshots,
        view_snapshots,
        sql_function_snapshots,
        catalog_snapshot,
        restart,
    } = pending
    else {
        state.data = pending;
        return;
    };
    let worker_engine = engine.session_portal_worker_engine(
        table_snapshots,
        view_snapshots,
        sql_function_snapshots,
        catalog_snapshot,
        state.transaction_origin,
    );
    state.data = SessionPortalData::Streaming {
        worker: crate::sql::start_session_portal_worker(worker_engine, query, params),
        materialized: None,
        eof: false,
        restart,
    };
}

fn stream_next_portal_row(
    engine: &Engine,
    state: &mut SessionPortalState,
) -> Result<bool, SQLError> {
    start_streaming_portal(engine, state);
    let SessionPortalData::Streaming {
        worker,
        materialized,
        eof,
        ..
    } = &mut state.data
    else {
        return Ok(false);
    };
    if *eof {
        return Ok(false);
    }
    engine.cancellation_token().check()?;
    let _ = worker
        .requests
        .send(crate::SessionPortalWorkerRequest::Next);
    loop {
        match worker.responses.recv() {
            Ok(crate::SessionPortalWorkerResponse::Started {
                columns,
                column_types,
            }) => {
                let schema =
                    uqa_execution::RowSchema::with_types(columns.clone(), column_types.clone());
                let rows = uqa_execution::IndexedSpill::new(schema)
                    .map_err(crate::sql::map_physical_exec_error)?;
                *materialized = Some(SessionPortalMaterialization {
                    columns,
                    column_types,
                    rows,
                });
            }
            Ok(crate::SessionPortalWorkerResponse::Row(values)) => {
                let output = materialized.as_mut().ok_or_else(|| {
                    SQLError::Internal("cursor worker returned a row before metadata".into())
                })?;
                output
                    .rows
                    .push(&uqa_execution::PhysicalRow::from_values(values))
                    .map_err(crate::sql::map_physical_exec_error)?;
                return Ok(true);
            }
            Ok(crate::SessionPortalWorkerResponse::Eof) => {
                *eof = true;
                return Ok(false);
            }
            Ok(crate::SessionPortalWorkerResponse::Error(error)) => {
                *eof = true;
                return Err(error);
            }
            Err(_) => {
                *eof = true;
                return Err(SQLError::Internal(
                    "cursor worker stopped without completing the query".into(),
                ));
            }
        }
    }
}

pub(super) fn ensure_portal_rows_for_fetch(
    engine: &Engine,
    state: &mut SessionPortalState,
    mut direction: CursorDirection,
    mut count: i64,
) -> Result<(), SQLError> {
    if count < 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward
        )
    {
        count = count.checked_neg().ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        direction = match direction {
            CursorDirection::Forward => CursorDirection::Backward,
            CursorDirection::Backward => CursorDirection::Forward,
            _ => unreachable!(),
        };
    }
    if count == 0 {
        return Ok(());
    }
    let required = match direction {
        CursorDirection::Forward if count == i64::MAX => None,
        CursorDirection::Forward => {
            let start = match state.position {
                SessionPortalPosition::BeforeFirst => 0,
                SessionPortalPosition::OnRow(position) => position,
                SessionPortalPosition::AfterLast => return Ok(()),
            };
            Some(
                start.saturating_add(usize::try_from(count).map_err(|_| SQLError::Routine {
                    sqlstate: "22003".into(),
                    message: "cursor position is out of range".into(),
                })?),
            )
        }
        CursorDirection::Absolute if count < 0 => {
            require_scroll(state)?;
            None
        }
        CursorDirection::Absolute => {
            Some(usize::try_from(count).map_err(|_| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "cursor position is out of range".into(),
            })?)
        }
        CursorDirection::Relative if count > 0 => {
            let current = match state.position {
                SessionPortalPosition::BeforeFirst => 0,
                SessionPortalPosition::OnRow(position) => position,
                SessionPortalPosition::AfterLast => return Ok(()),
            };
            Some(
                current.saturating_add(usize::try_from(count).map_err(|_| SQLError::Routine {
                    sqlstate: "22003".into(),
                    message: "cursor position is out of range".into(),
                })?),
            )
        }
        CursorDirection::Backward | CursorDirection::Relative => return Ok(()),
    };
    loop {
        if required.is_some_and(|required| portal_row_count(state) >= required) {
            return Ok(());
        }
        if !stream_next_portal_row(engine, state)? {
            return Ok(());
        }
    }
}

pub(super) fn materialize_portal_to_end(
    engine: &Engine,
    state: &mut SessionPortalState,
) -> Result<(), SQLError> {
    let streaming = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    state.data = match streaming {
        SessionPortalData::Streaming {
            restart: Some(restart),
            ..
        } => SessionPortalData::Pending {
            query: restart.query,
            params: restart.params,
            table_snapshots: restart.table_snapshots,
            view_snapshots: restart.view_snapshots,
            sql_function_snapshots: restart.sql_function_snapshots,
            catalog_snapshot: restart.catalog_snapshot,
            restart: None,
        },
        other => other,
    };
    while stream_next_portal_row(engine, state)? {}
    let streaming = std::mem::replace(
        &mut state.data,
        SessionPortalData::Result(SQLResult::empty()),
    );
    match streaming {
        SessionPortalData::Streaming {
            materialized: Some(materialized),
            ..
        } => state.data = SessionPortalData::Indexed(materialized),
        SessionPortalData::Streaming {
            materialized: None, ..
        } => {
            return Err(SQLError::Internal(
                "cursor worker completed without result metadata".into(),
            ));
        }
        other => state.data = other,
    }
    Ok(())
}

fn portal_row_count(state: &SessionPortalState) -> usize {
    match &state.data {
        SessionPortalData::Pending { .. } => 0,
        SessionPortalData::Result(result) => result.rows.len(),
        SessionPortalData::Indexed(result) => {
            usize::try_from(result.rows.len()).unwrap_or(usize::MAX)
        }
        SessionPortalData::Streaming { materialized, .. } => {
            materialized.as_ref().map_or(0, |result| {
                usize::try_from(result.rows.len()).unwrap_or(usize::MAX)
            })
        }
    }
}

pub(super) fn fetch_indices(
    state: &mut SessionPortalState,
    mut direction: CursorDirection,
    mut count: i64,
    move_only: bool,
) -> Result<Vec<usize>, SQLError> {
    if count < 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward
        )
    {
        count = count.checked_neg().ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        direction = match direction {
            CursorDirection::Forward => CursorDirection::Backward,
            CursorDirection::Backward => CursorDirection::Forward,
            _ => unreachable!(),
        };
    }
    // PostgreSQL answers MOVE {FORWARD|BACKWARD|RELATIVE} 0 from the current portal state without asking the executor to rescan. It therefore works for NO SCROLL cursors and reports 1 exactly when the cursor is on a row.
    if move_only
        && count == 0
        && matches!(
            direction,
            CursorDirection::Forward | CursorDirection::Backward | CursorDirection::Relative
        )
    {
        return Ok(current_row(state).into_iter().collect());
    }
    match direction {
        CursorDirection::Forward => fetch_forward(state, count),
        CursorDirection::Backward => fetch_backward(state, count),
        CursorDirection::Absolute => fetch_absolute(state, count),
        CursorDirection::Relative => fetch_relative(state, count),
    }
}

fn require_scroll(state: &SessionPortalState) -> Result<(), SQLError> {
    if state.scrollable {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: "cursor can only scan forward".into(),
        })
    }
}

fn current_row(state: &SessionPortalState) -> Option<usize> {
    match state.position {
        SessionPortalPosition::OnRow(position) => Some(position - 1),
        SessionPortalPosition::BeforeFirst | SessionPortalPosition::AfterLast => None,
    }
}

fn fetch_forward(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    if count == 0 {
        if current_row(state).is_some() {
            require_scroll(state)?;
        }
        return Ok(current_row(state).into_iter().collect());
    }
    if state.position == SessionPortalPosition::AfterLast {
        return Ok(Vec::new());
    }
    let start = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => position,
        SessionPortalPosition::AfterLast => unreachable!(),
    };
    let available = portal_row_count(state).saturating_sub(start);
    let requested = if count == i64::MAX {
        available
    } else {
        usize::try_from(count).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?
    };
    let fetched = available.min(requested);
    if fetched != 0 {
        state.position = SessionPortalPosition::OnRow(start + fetched);
    }
    if count == i64::MAX || fetched < requested {
        state.position = SessionPortalPosition::AfterLast;
    }
    Ok((start..start + fetched).collect())
}

fn fetch_backward(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    require_scroll(state)?;
    if count == 0 {
        return Ok(current_row(state).into_iter().collect());
    }
    if state.position == SessionPortalPosition::BeforeFirst {
        return Ok(Vec::new());
    }
    let conceptual_position = match state.position {
        SessionPortalPosition::BeforeFirst => unreachable!(),
        SessionPortalPosition::OnRow(position) => position,
        SessionPortalPosition::AfterLast => portal_row_count(state).saturating_add(1),
    };
    let available = conceptual_position.saturating_sub(1);
    let requested = if count == i64::MAX {
        available
    } else {
        usize::try_from(count).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?
    };
    let fetched = available.min(requested);
    let indices = (0..fetched)
        .map(|offset| conceptual_position - offset - 2)
        .collect::<Vec<_>>();
    if fetched != 0 {
        state.position = SessionPortalPosition::OnRow(conceptual_position - fetched);
    }
    if count == i64::MAX || fetched < requested {
        state.position = SessionPortalPosition::BeforeFirst;
    }
    Ok(indices)
}

fn fetch_absolute(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    let row_count = i128::try_from(portal_row_count(state)).unwrap_or(i128::MAX);
    let target = match count.cmp(&0) {
        std::cmp::Ordering::Greater => i128::from(count),
        std::cmp::Ordering::Less => row_count + 1 + i128::from(count),
        std::cmp::Ordering::Equal => 0,
    };
    let current = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => i128::try_from(position).unwrap_or(i128::MAX),
        SessionPortalPosition::AfterLast => row_count + 1,
    };
    let requires_scroll = count < 0
        || (count == 0 && state.position != SessionPortalPosition::BeforeFirst)
        || (count > 0 && target <= current);
    if !state.scrollable && requires_scroll {
        return require_scroll(state).map(|()| Vec::new());
    }
    position_at(state, target, row_count)
}

fn fetch_relative(state: &mut SessionPortalState, count: i64) -> Result<Vec<usize>, SQLError> {
    let row_count = i128::try_from(portal_row_count(state)).unwrap_or(i128::MAX);
    let current = match state.position {
        SessionPortalPosition::BeforeFirst => 0,
        SessionPortalPosition::OnRow(position) => i128::try_from(position).unwrap_or(i128::MAX),
        SessionPortalPosition::AfterLast => row_count + 1,
    };
    if count == 0 {
        return fetch_forward(state, 0);
    }
    if count < 0 {
        require_scroll(state)?;
    }
    position_at(state, current + i128::from(count), row_count)
}

fn position_at(
    state: &mut SessionPortalState,
    target: i128,
    row_count: i128,
) -> Result<Vec<usize>, SQLError> {
    if target <= 0 {
        state.position = SessionPortalPosition::BeforeFirst;
        return Ok(Vec::new());
    }
    if target > row_count {
        state.position = SessionPortalPosition::AfterLast;
        return Ok(Vec::new());
    }
    let row = usize::try_from(target - 1).map_err(|_| SQLError::Routine {
        sqlstate: "22003".into(),
        message: "cursor position is out of range".into(),
    })?;
    state.position = SessionPortalPosition::OnRow(row + 1);
    Ok(vec![row])
}

pub(super) fn select_portal_rows(
    state: &mut SessionPortalState,
    indices: &[usize],
) -> Result<SQLResult, SQLError> {
    let empty_columns = state.columns.clone();
    let empty_column_types = state.column_types.clone();
    let empty = || {
        SQLResult::from_typed_rows_with_positions(
            empty_columns.clone(),
            empty_column_types.clone(),
            Vec::new(),
            Some(Vec::new()),
        )
    };
    match &mut state.data {
        SessionPortalData::Pending { .. }
        | SessionPortalData::Streaming {
            materialized: None, ..
        } if indices.is_empty() => Ok(empty()),
        SessionPortalData::Pending { .. } => Err(SQLError::Internal(
            "session portal remained pending during FETCH".into(),
        )),
        SessionPortalData::Result(result) => Ok(select_result_rows(result, indices)),
        SessionPortalData::Indexed(result)
        | SessionPortalData::Streaming {
            materialized: Some(result),
            ..
        } => select_indexed_rows(result, indices),
        SessionPortalData::Streaming { .. } => Err(SQLError::Internal(
            "cursor row materialization is absent".into(),
        )),
    }
}

fn select_result_rows(result: &SQLResult, indices: &[usize]) -> SQLResult {
    let rows = indices
        .iter()
        .map(|&index| result.rows[index].clone())
        .collect();
    let positional_rows = result.positional_rows.as_ref().map(|rows| {
        indices
            .iter()
            .map(|&index| rows[index].clone())
            .collect::<Vec<_>>()
    });
    SQLResult {
        columns: result.columns.clone(),
        column_types: result.column_types.clone(),
        rows,
        positional_rows,
        affected_rows: 0,
    }
}

fn select_indexed_rows(
    result: &mut SessionPortalMaterialization,
    indices: &[usize],
) -> Result<SQLResult, SQLError> {
    let schema = result.rows.row_schema().clone();
    let mut positional_rows = Vec::with_capacity(indices.len());
    for &index in indices {
        let index = u64::try_from(index).map_err(|_| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "cursor position is out of range".into(),
        })?;
        let row = result
            .rows
            .get(index)
            .map_err(crate::sql::map_physical_exec_error)?;
        let view = schema.view(&row);
        positional_rows.push(
            (0..result.columns.len())
                .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
                .collect(),
        );
    }
    Ok(SQLResult::from_typed_rows_with_positions(
        result.columns.clone(),
        result.column_types.clone(),
        vec![uqa_sql::ResultRow::new(); positional_rows.len()],
        Some(positional_rows),
    ))
}
