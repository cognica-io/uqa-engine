//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal row-streaming worker.

use uqa_core::Value;
use uqa_planner::QueryPlan;
use uqa_sql::{SQLError, SQLParam, SQLResult};

use crate::{
    Engine, SessionPortalDeclaration, SessionPortalWorker, SessionPortalWorkerRequest,
    SessionPortalWorkerResponse,
};

use super::{query_has_row_locks, select};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PortalDeclarationContext {
    Sql,
    PLpgSQL,
}

pub(super) fn declare_session_portal(
    engine: &Engine,
    params: &[SQLParam],
    name: &str,
    binary: bool,
    scroll: Option<bool>,
    hold: bool,
    query: &QueryPlan,
) -> Result<SQLResult, SQLError> {
    prepare_session_portal(
        engine,
        params,
        name,
        binary,
        scroll,
        hold,
        query,
        PortalDeclarationContext::Sql,
    )?;
    Ok(SQLResult::empty())
}

pub(super) fn open_plpgsql_session_portal(
    engine: &Engine,
    params: &[SQLParam],
    name: &str,
    scroll: Option<bool>,
    query: &QueryPlan,
) -> Result<(), SQLError> {
    prepare_session_portal(
        engine,
        params,
        name,
        false,
        scroll,
        false,
        query,
        PortalDeclarationContext::PLpgSQL,
    )
}

pub(super) fn ensure_plpgsql_session_portal_available(
    engine: &Engine,
    name: &str,
) -> Result<(), SQLError> {
    engine
        .ensure_session_portal_available(name)
        .map_err(|error| {
            if error.sqlstate() == Some("42P03") {
                SQLError::Routine {
                    sqlstate: "42P03".into(),
                    message: format!("cursor \"{name}\" already in use"),
                }
            } else {
                error
            }
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SQL and PL/pgSQL portal contracts explicit"
)]
fn prepare_session_portal(
    engine: &Engine,
    params: &[SQLParam],
    name: &str,
    binary: bool,
    scroll: Option<bool>,
    hold: bool,
    query: &QueryPlan,
    context: PortalDeclarationContext,
) -> Result<(), SQLError> {
    if context == PortalDeclarationContext::Sql && !hold && !engine.in_transaction_block() {
        return Err(SQLError::Routine {
            sqlstate: "25P01".into(),
            message: "DECLARE CURSOR can only be used in transaction blocks".into(),
        });
    }
    if context == PortalDeclarationContext::PLpgSQL {
        ensure_plpgsql_session_portal_available(engine, name)?;
    } else {
        engine.ensure_session_portal_available(name)?;
    }
    let has_row_locks = query_has_row_locks(query);
    if has_row_locks && hold {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "DECLARE CURSOR WITH HOLD ... FOR UPDATE is not supported".into(),
        });
    }
    if has_row_locks && scroll == Some(true) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: if context == PortalDeclarationContext::PLpgSQL {
                "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into()
            } else {
                "DECLARE SCROLL CURSOR ... FOR UPDATE is not supported".into()
            },
        });
    }
    select::lock_query_relations(engine, query)?;
    let ctes = select::CteScope::new_for_current_routine(engine);
    let schema = select::analyze_query_plan_schema(engine, query, params, &ctes, None)?;
    select::validate_query_row_locks(engine, query, params)?;
    engine.open_pending_session_portal(SessionPortalDeclaration {
        name: name.to_string(),
        query: query.clone(),
        params: params.to_vec(),
        columns: schema.columns().to_vec(),
        column_types: schema.column_types().to_vec(),
        scrollable: scroll.unwrap_or(!has_row_locks),
        holdable: hold,
        binary,
    })?;
    Ok(())
}

struct SessionPortalRowConsumer {
    requests: std::sync::mpsc::Receiver<SessionPortalWorkerRequest>,
    responses: std::sync::mpsc::Sender<SessionPortalWorkerResponse>,
    initial_next: std::cell::Cell<bool>,
    closed: std::cell::Cell<bool>,
    public_width: std::cell::Cell<usize>,
}

impl select::QueryRowConsumer for SessionPortalRowConsumer {
    fn begin(
        &self,
        _engine: &Engine,
        columns: &[String],
        schema: &uqa_execution::RowSchema,
    ) -> Result<(), SQLError> {
        self.public_width.set(columns.len());
        let column_types = columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                if schema.columns().get(position) == Some(column) {
                    schema.column_type(position).cloned()
                } else {
                    schema
                        .position(column)
                        .and_then(|position| schema.column_type(position).cloned())
                }
            })
            .collect();
        self.responses
            .send(SessionPortalWorkerResponse::Started {
                columns: columns.to_vec(),
                column_types,
            })
            .map_err(|_| SQLError::Internal("cursor consumer disconnected before startup".into()))
    }

    fn consume(
        &self,
        _engine: &Engine,
        row: uqa_execution::OwnedPhysicalRow,
    ) -> Result<select::QueryConsumerControl, SQLError> {
        let request = if self.initial_next.replace(false) {
            SessionPortalWorkerRequest::Next
        } else {
            match self.requests.recv() {
                Ok(request) => request,
                Err(_) => SessionPortalWorkerRequest::Close,
            }
        };
        if matches!(request, SessionPortalWorkerRequest::Close) {
            self.closed.set(true);
            return Ok(select::QueryConsumerControl::Stop);
        }
        let view = row.view();
        let values = (0..self.public_width.get())
            .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
            .collect();
        if self
            .responses
            .send(SessionPortalWorkerResponse::Row(values))
            .is_err()
        {
            self.closed.set(true);
            return Ok(select::QueryConsumerControl::Stop);
        }
        match self.requests.recv() {
            Ok(SessionPortalWorkerRequest::Next) => {
                self.initial_next.set(true);
                Ok(select::QueryConsumerControl::Continue)
            }
            Ok(SessionPortalWorkerRequest::Close) | Err(_) => {
                self.closed.set(true);
                Ok(select::QueryConsumerControl::Stop)
            }
        }
    }
}

pub(crate) fn start_session_portal_worker(
    engine: Engine,
    query: QueryPlan,
    params: Vec<SQLParam>,
) -> SessionPortalWorker {
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let (response_tx, response_rx) = std::sync::mpsc::channel();
    let join = std::thread::spawn(move || {
        let _statement_gate = engine.runtime.statement_gate.delegate_to_current_thread();
        let first = match request_rx.recv() {
            Ok(SessionPortalWorkerRequest::Next) => true,
            Ok(SessionPortalWorkerRequest::Close) | Err(_) => return,
        };
        let consumer = std::rc::Rc::new(SessionPortalRowConsumer {
            requests: request_rx,
            responses: response_tx.clone(),
            initial_next: std::cell::Cell::new(first),
            closed: std::cell::Cell::new(false),
            public_width: std::cell::Cell::new(0),
        });
        let mut ctes = select::CteScope::new_for_current_routine(&engine);
        ctes.enable_command_progress_streaming();
        let result = select::execute_query_plan_output(
            &engine,
            &query,
            &params,
            &mut ctes,
            select::QueryOutputMode::RowConsumer(consumer.clone()),
        );
        match result {
            Ok(_) if !consumer.closed.get() => {
                let _ = response_tx.send(SessionPortalWorkerResponse::Eof);
            }
            Ok(_) => {}
            Err(error) => {
                let _ = response_tx.send(SessionPortalWorkerResponse::Error(error));
            }
        }
    });
    SessionPortalWorker {
        requests: request_tx,
        responses: response_rx,
        join: Some(join),
    }
}
