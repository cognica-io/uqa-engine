//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native directional row streaming and portal worker channels.

#[cfg(test)]
mod tests;

use crate::{
    query::consumer::QueryConsumerControl, statement::context::queries::StatementQueryContexts,
};
use uqa_core::Value;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

pub enum SessionPortalWorkerRequest {
    Step(crate::PhysicalScanDirection),
    Rewind,
    Close,
}

pub enum SessionPortalWorkerResponse {
    Started {
        columns: Vec<String>,
        column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    },
    Row(Vec<Value>),
    Eof,
    Rewound,
    Error(SQLError),
}

pub struct SessionPortalWorker {
    pub requests: std::sync::mpsc::Sender<SessionPortalWorkerRequest>,
    pub responses: std::sync::mpsc::Receiver<SessionPortalWorkerResponse>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SessionPortalWorker {
    fn drop(&mut self) {
        let _ = self.requests.send(SessionPortalWorkerRequest::Close);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct SessionPortalRowConsumer {
    requests: std::sync::mpsc::Receiver<SessionPortalWorkerRequest>,
    responses: std::sync::mpsc::Sender<SessionPortalWorkerResponse>,
    direction: std::cell::Cell<crate::PhysicalScanDirection>,
    directional: bool,
    closed: std::cell::Cell<bool>,
    public_width: std::cell::Cell<usize>,
}

impl SessionPortalRowConsumer {
    fn wait_for_request(&self) -> Result<QueryConsumerControl, SQLError> {
        match self.requests.recv() {
            Ok(SessionPortalWorkerRequest::Step(direction)) => {
                self.direction.set(direction);
                Ok(QueryConsumerControl::Continue)
            }
            Ok(SessionPortalWorkerRequest::Rewind) if self.directional => {
                Ok(QueryConsumerControl::Rewind)
            }
            Ok(SessionPortalWorkerRequest::Rewind) => Err(SQLError::Internal(
                "forward-only cursor worker received a rewind request".into(),
            )),
            Ok(SessionPortalWorkerRequest::Close) | Err(_) => {
                self.closed.set(true);
                Ok(QueryConsumerControl::Stop)
            }
        }
    }

    fn respond_and_wait(
        &self,
        response: SessionPortalWorkerResponse,
    ) -> Result<QueryConsumerControl, SQLError> {
        if self.responses.send(response).is_err() {
            self.closed.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        self.wait_for_request()
    }
}

impl crate::query::consumer::QueryRowConsumer for SessionPortalRowConsumer {
    fn begin(&self, columns: &[String], schema: &crate::RowSchema) -> Result<(), SQLError> {
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

    fn consume(&self, row: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let view = row.view();
        let values = (0..self.public_width.get())
            .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
            .collect();
        self.respond_and_wait(SessionPortalWorkerResponse::Row(values))
    }

    fn uses_directional_scan(&self) -> bool {
        self.directional
    }

    fn scan_direction(&self) -> crate::PhysicalScanDirection {
        self.direction.get()
    }

    fn direction_exhausted(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(SessionPortalWorkerResponse::Eof)
    }

    fn rewound(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(SessionPortalWorkerResponse::Rewound)
    }
}

pub fn run<S: Clone + Send + Sync + 'static>(
    queries: &dyn StatementQueryContexts<S>,
    query: &QueryPlan,
    params: &[SQLParam],
    directional: bool,
    request_rx: std::sync::mpsc::Receiver<SessionPortalWorkerRequest>,
    response_tx: std::sync::mpsc::Sender<SessionPortalWorkerResponse>,
) {
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
            Ok(SessionPortalWorkerRequest::Rewind | SessionPortalWorkerRequest::Close) | Err(_) => {
                return;
            }
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
    let mut ctes = queries.statement_scope(None);
    ctes.enable_command_progress_streaming();
    if directional {
        ctes.enable_backwards_scanning();
    }
    let result = crate::query::statement::execute_query_plan_output(
        &queries.query_context(),
        query,
        params,
        &mut ctes,
        crate::query::statement::consumer::QueryOutputMode::physical_consumer(consumer.clone()),
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
}
