//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exchange one bounded row request and optionally retain it for a holdable cursor.

use super::{
    start_streaming_portal, Engine, SQLError, SessionPortalData, SessionPortalMaterialization,
    SessionPortalState, Value,
};
use crate::{SessionPortalWorkerRequest as Request, SessionPortalWorkerResponse as Response};
use std::num::NonZeroUsize;
use uqa_execution::{statement::portal::worker::WorkerBatch, PhysicalScanDirection};

pub(super) fn read_batch(
    engine: &Engine,
    state: &mut SessionPortalState,
    direction: PhysicalScanDirection,
    demand: NonZeroUsize,
    capture: bool,
) -> Result<WorkerBatch<Vec<Value>>, SQLError> {
    start_streaming_portal(engine, state);
    let SessionPortalData::Streaming {
        worker,
        eof,
        forward_rows,
        ..
    } = &mut state.data
    else {
        return Err(SQLError::Internal("cursor has no query worker".into()));
    };
    engine.cancellation_token().check()?;
    if *eof && !state.scrollable {
        return Ok(WorkerBatch::exhausted());
    }
    worker
        .requests
        .send(Request::StepBatch {
            direction,
            max_rows: demand,
            capture,
            diagnostics: Some(
                engine
                    .runtime
                    .diagnostics
                    .request_with_control(engine.query_retention_control()?),
            ),
        })
        .map_err(|_| SQLError::Internal("cursor worker stopped before a row request".into()))?;
    loop {
        match worker.responses.recv() {
            Ok(Response::Started { schema }) => state.schema = schema,
            Ok(Response::Batch(batch)) => {
                if !state.scrollable {
                    *eof = batch.exhausted;
                    *forward_rows = forward_rows
                        .checked_add(batch.processed)
                        .ok_or_else(super::cursor_position_error)?;
                }
                return Ok(batch);
            }
            Ok(Response::Error(error)) => {
                *eof = true;
                return Err(error);
            }
            Ok(_) => {
                return Err(SQLError::Internal(
                    "cursor worker returned an invalid batch response".into(),
                ))
            }
            Err(_) => {
                *eof = true;
                return Err(SQLError::Internal(
                    "cursor worker stopped without completing the row request".into(),
                ));
            }
        }
    }
}

pub(super) fn append_batch(
    engine: &Engine,
    state: &mut SessionPortalState,
    max_rows: usize,
) -> Result<bool, SQLError> {
    let preceding = match &state.data {
        SessionPortalData::Pending { .. } => 0,
        SessionPortalData::Streaming {
            eof: true,
            materialized: Some(_),
            ..
        } => return Ok(false),
        SessionPortalData::Streaming { forward_rows, .. } => *forward_rows,
        _ => return Ok(false),
    };
    let rows = NonZeroUsize::new(max_rows.min(uqa_execution::DEFAULT_BATCH_SIZE))
        .expect("at least one row requested");
    let batch = read_batch(engine, state, PhysicalScanDirection::Forward, rows, true)?;
    let SessionPortalData::Streaming {
        materialized, eof, ..
    } = &mut state.data
    else {
        unreachable!("active worker")
    };
    if materialized.is_none() {
        let rows = uqa_execution::IndexedSpill::new(state.schema.clone())
            .map_err(uqa_execution::query::projection::physical_exec_error)?;
        *materialized = Some(SessionPortalMaterialization { preceding, rows });
    }
    let output = materialized.as_mut().expect("holdable cursor store opened");
    *eof = batch.exhausted;
    let processed = batch.processed;
    batch.visit(|values| {
        output
            .rows
            .push(&uqa_execution::PhysicalRow::from_values(values))
            .map_err(uqa_execution::query::projection::physical_exec_error)
    })?;
    Ok(processed != 0)
}
