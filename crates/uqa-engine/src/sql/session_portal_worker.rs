//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal row-streaming worker.

use uqa_core::Value;
use uqa_planner::{CommandPlan, QueryPlan, UnifiedPlan};
use uqa_sql::{SQLError, SQLParam, SQLResult};

use crate::{
    Engine, SessionPortalCommandDeclaration, SessionPortalDeclaration, SessionPortalWorker,
    SessionPortalWorkerRequest, SessionPortalWorkerResponse,
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
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => prepare_session_portal(
            engine,
            params,
            name,
            false,
            scroll,
            false,
            query,
            PortalDeclarationContext::PLpgSQL,
        ),
        UnifiedPlan::Command(command) => {
            open_plpgsql_command_portal(engine, params, name, scroll, command)
        }
    }
}

fn open_plpgsql_command_portal(
    engine: &Engine,
    params: &[SQLParam],
    name: &str,
    scroll: Option<bool>,
    command: &CommandPlan,
) -> Result<(), SQLError> {
    let schema = match command {
        CommandPlan::Insert(_)
        | CommandPlan::Update(_)
        | CommandPlan::Delete(_)
        | CommandPlan::Merge(_) => {
            super::dml::cursor_command_returning_schema(engine, command, params)?
        }
        CommandPlan::Call { name, args } => {
            super::analyze_call_result_schema(engine, name, args, params)?
        }
        CommandPlan::ShowVariable { name } => {
            engine.session_execution_view().show_variable(name)?;
            Some(uqa_execution::RowSchema::with_types(
                vec![name.clone()],
                vec![Some(uqa_sql::ColumnType::Text)],
            ))
        }
        CommandPlan::Explain { body, format, .. } => {
            validate_explain_cursor_body(engine, params, body)?;
            let result = select::run_explain(body, false, format.as_deref(), None)?;
            Some(uqa_execution::RowSchema::with_types(
                result.columns,
                result.column_types,
            ))
        }
        _ => None,
    }
    .ok_or_else(|| cannot_open_command_cursor(command))?;
    if scroll == Some(true) && matches!(command, CommandPlan::Merge(_)) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into(),
        });
    }
    let null_returning_values = scroll == Some(true)
        && matches!(
            command,
            CommandPlan::Insert(_) | CommandPlan::Update(_) | CommandPlan::Delete(_)
        );
    engine.open_pending_command_session_portal(SessionPortalCommandDeclaration {
        name: name.to_string(),
        command: Box::new(command.clone()),
        params: params.to_vec(),
        columns: schema.columns().to_vec(),
        column_types: schema.column_types().to_vec(),
        scrollable: scroll.unwrap_or(false),
        null_returning_values,
    })
}

fn validate_explain_cursor_body(
    engine: &Engine,
    params: &[SQLParam],
    body: &UnifiedPlan,
) -> Result<(), SQLError> {
    match body {
        UnifiedPlan::Query(query) => {
            select::lock_query_relations(engine, query)?;
            let ctes = select::CteScope::new_for_current_routine(engine);
            select::analyze_query_plan_schema(engine, query, params, &ctes, None)?;
            Ok(())
        }
        UnifiedPlan::Command(command) => {
            let _ = super::dml::cursor_command_returning_schema(engine, command, params)?;
            Ok(())
        }
    }
}

fn cannot_open_command_cursor(command: &CommandPlan) -> SQLError {
    let tag = match command {
        CommandPlan::Insert(_) => "INSERT",
        CommandPlan::Update(_) => "UPDATE",
        CommandPlan::Delete(_) => "DELETE",
        CommandPlan::Merge(_) => "MERGE",
        CommandPlan::Call { .. } => "CALL",
        CommandPlan::ShowVariable { .. } => "SHOW",
        CommandPlan::Explain { .. } => "EXPLAIN",
        _ => command.name(),
    };
    SQLError::Routine {
        sqlstate: "42P11".into(),
        message: format!("cannot open {tag} query as cursor"),
    }
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
    direction: std::cell::Cell<uqa_execution::PhysicalScanDirection>,
    directional: bool,
    closed: std::cell::Cell<bool>,
    public_width: std::cell::Cell<usize>,
}

impl SessionPortalRowConsumer {
    fn wait_for_request(&self) -> Result<select::QueryConsumerControl, SQLError> {
        match self.requests.recv() {
            Ok(SessionPortalWorkerRequest::Step(direction)) => {
                self.direction.set(direction);
                Ok(select::QueryConsumerControl::Continue)
            }
            Ok(SessionPortalWorkerRequest::Rewind) if self.directional => {
                Ok(select::QueryConsumerControl::Rewind)
            }
            Ok(SessionPortalWorkerRequest::Rewind) => Err(SQLError::Internal(
                "forward-only cursor worker received a rewind request".into(),
            )),
            Ok(SessionPortalWorkerRequest::Close) | Err(_) => {
                self.closed.set(true);
                Ok(select::QueryConsumerControl::Stop)
            }
        }
    }

    fn respond_and_wait(
        &self,
        response: SessionPortalWorkerResponse,
    ) -> Result<select::QueryConsumerControl, SQLError> {
        if self.responses.send(response).is_err() {
            self.closed.set(true);
            return Ok(select::QueryConsumerControl::Stop);
        }
        self.wait_for_request()
    }
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
        let view = row.view();
        let values = (0..self.public_width.get())
            .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
            .collect();
        self.respond_and_wait(SessionPortalWorkerResponse::Row(values))
    }

    fn uses_directional_scan(&self) -> bool {
        self.directional
    }

    fn scan_direction(&self) -> uqa_execution::PhysicalScanDirection {
        self.direction.get()
    }

    fn direction_exhausted(
        &self,
        _engine: &Engine,
    ) -> Result<select::QueryConsumerControl, SQLError> {
        self.respond_and_wait(SessionPortalWorkerResponse::Eof)
    }

    fn rewound(&self, _engine: &Engine) -> Result<select::QueryConsumerControl, SQLError> {
        self.respond_and_wait(SessionPortalWorkerResponse::Rewound)
    }
}

pub(crate) fn start_session_portal_worker(
    engine: Engine,
    query: QueryPlan,
    params: Vec<SQLParam>,
    directional: bool,
) -> SessionPortalWorker {
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let (response_tx, response_rx) = std::sync::mpsc::channel();
    let join = std::thread::spawn(move || {
        let _statement_gate = engine.runtime.statement_gate.delegate_to_current_thread();
        let first_direction = loop {
            match request_rx.recv() {
                Ok(SessionPortalWorkerRequest::Step(direction)) => break direction,
                Ok(SessionPortalWorkerRequest::Rewind) if directional => {
                    if response_tx
                        .send(SessionPortalWorkerResponse::Rewound)
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(SessionPortalWorkerRequest::Rewind) => return,
                Ok(SessionPortalWorkerRequest::Close) | Err(_) => return,
            }
        };
        let consumer = std::rc::Rc::new(SessionPortalRowConsumer {
            requests: request_rx,
            responses: response_tx.clone(),
            direction: std::cell::Cell::new(first_direction),
            directional,
            closed: std::cell::Cell::new(false),
            public_width: std::cell::Cell::new(0),
        });
        let mut ctes = select::CteScope::new_for_current_routine(&engine);
        ctes.enable_command_progress_streaming();
        if directional {
            ctes.enable_backwards_scanning();
        }
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
