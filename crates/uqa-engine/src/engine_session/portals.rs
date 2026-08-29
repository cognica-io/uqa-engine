//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal lifecycle shared by PL/pgSQL routine activations.

use crate::{Engine, SQLError, SQLResult, SessionPortalPosition, SessionPortalState, Value};
use uqa_sql::ast::{CursorDirection, FetchCursorStmt};

impl Engine {
    pub(crate) fn allocate_session_portal_name(&self) -> String {
        let mut next = self.session.next_portal_id.lock();
        let name = format!("<unnamed portal {}>", *next);
        *next += 1;
        name
    }

    pub(crate) fn open_session_portal(
        &self,
        name: String,
        result: SQLResult,
    ) -> Result<(), SQLError> {
        self.open_session_portal_with_options(name, result, false, false)
    }

    pub(crate) fn open_session_portal_with_options(
        &self,
        name: String,
        result: SQLResult,
        scrollable: bool,
        holdable: bool,
    ) -> Result<(), SQLError> {
        let mut portals = self.session.portals.lock();
        if portals.contains_key(&name) {
            return Err(cursor_error(&name, "already exists", "42P03"));
        }
        portals.insert(
            name,
            SessionPortalState {
                result,
                position: SessionPortalPosition::BeforeFirst,
                scrollable,
                holdable,
            },
        );
        Ok(())
    }

    pub(crate) fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().contains_key(name) {
            return Err(cursor_error(name, "already exists", "42P03"));
        }
        Ok(())
    }

    pub(crate) fn fetch_session_portal_next(
        &self,
        name: &str,
    ) -> Result<(Vec<String>, Option<Vec<Value>>), SQLError> {
        let result = self.fetch_session_portal(&FetchCursorStmt {
            name: name.to_string(),
            direction: CursorDirection::Forward,
            count: 1,
            move_only: false,
        })?;
        let values = result.rows.first().map(|_| {
            (0..result.columns.len())
                .map(|column| result.value_at(0, column).cloned().unwrap_or(Value::Null))
                .collect()
        });
        Ok((result.columns, values))
    }

    pub(crate) fn fetch_session_portal(
        &self,
        fetch: &FetchCursorStmt,
    ) -> Result<SQLResult, SQLError> {
        let mut portals = self.session.portals.lock();
        let state = portals
            .get_mut(&fetch.name)
            .ok_or_else(|| cursor_error(&fetch.name, "does not exist", "34000"))?;
        let indices = fetch_indices(state, fetch.direction, fetch.count, fetch.move_only)?;
        if fetch.move_only {
            return Ok(SQLResult::from_affected(indices.len() as u64));
        }
        Ok(select_portal_rows(&state.result, &indices))
    }

    pub(crate) fn close_session_portal(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().remove(name).is_none() {
            return Err(cursor_error(name, "does not exist", "34000"));
        }
        Ok(())
    }

    pub(crate) fn close_all_session_portals(&self) {
        self.session.portals.lock().clear();
    }
}

fn fetch_indices(
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
    let available = state.result.rows.len().saturating_sub(start);
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
        SessionPortalPosition::AfterLast => state.result.rows.len().saturating_add(1),
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
    let row_count = i128::try_from(state.result.rows.len()).unwrap_or(i128::MAX);
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
    let row_count = i128::try_from(state.result.rows.len()).unwrap_or(i128::MAX);
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

fn select_portal_rows(result: &SQLResult, indices: &[usize]) -> SQLResult {
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

fn cursor_error(name: &str, message: &str, sqlstate: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("cursor \"{name}\" {message}"),
    }
}
