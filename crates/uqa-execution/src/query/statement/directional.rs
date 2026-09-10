//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent directional query traversal over a session-owned worker task.
use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
use crate::{
    BackwardScanSupport, Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow,
    PhysicalScanDirection, RowSchema,
};
use std::{cell::Cell, rc::Rc};
use uqa_core::Value;
use uqa_sql::SQLError;

enum DirectionalPlanRequest {
    Step(PhysicalScanDirection),
    Rewind,
    Close,
}

enum DirectionalPlanResponse {
    Row(PhysicalRow),
    Eof,
    Rewound,
    Error(SQLError),
}

struct DirectionalPlanWorker {
    requests: std::sync::mpsc::Sender<DirectionalPlanRequest>,
    responses: std::sync::mpsc::Receiver<DirectionalPlanResponse>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DirectionalPlanWorker {
    fn drop(&mut self) {
        let _ = self.requests.send(DirectionalPlanRequest::Close);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct DirectionalPlanRowConsumer {
    requests: std::sync::mpsc::Receiver<DirectionalPlanRequest>,
    responses: std::sync::mpsc::Sender<DirectionalPlanResponse>,
    direction: Cell<PhysicalScanDirection>,
    closed: Cell<bool>,
    schema: RowSchema,
}

impl DirectionalPlanRowConsumer {
    fn wait_for_request(&self) -> Result<QueryConsumerControl, SQLError> {
        match self.requests.recv() {
            Ok(DirectionalPlanRequest::Step(direction)) => {
                self.direction.set(direction);
                Ok(QueryConsumerControl::Continue)
            }
            Ok(DirectionalPlanRequest::Rewind) => Ok(QueryConsumerControl::Rewind),
            Ok(DirectionalPlanRequest::Close) | Err(_) => {
                self.closed.set(true);
                Ok(QueryConsumerControl::Stop)
            }
        }
    }

    fn respond_and_wait(
        &self,
        response: DirectionalPlanResponse,
    ) -> Result<QueryConsumerControl, SQLError> {
        if self.responses.send(response).is_err() {
            self.closed.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        self.wait_for_request()
    }
}

impl QueryRowConsumer for DirectionalPlanRowConsumer {
    fn begin(&self, columns: &[String], _schema: &RowSchema) -> Result<(), SQLError> {
        if columns.len() != self.schema.len() {
            return Err(SQLError::TypeMismatch(format!(
                "directional set-operation branch width {} does not match declared width {}",
                columns.len(),
                self.schema.len()
            )));
        }
        Ok(())
    }

    fn consume(&self, row: crate::OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let view = row.view();
        let values = (0..self.schema.len())
            .map(|position| view.value_at(position).cloned().unwrap_or(Value::Null))
            .collect();
        self.respond_and_wait(DirectionalPlanResponse::Row(PhysicalRow::from_values(
            values,
        )))
    }

    fn uses_directional_scan(&self) -> bool {
        true
    }

    fn scan_direction(&self) -> PhysicalScanDirection {
        self.direction.get()
    }

    fn direction_exhausted(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(DirectionalPlanResponse::Eof)
    }

    fn rewound(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(DirectionalPlanResponse::Rewound)
    }
}

/// Run one query in the session state prepared by the caller. The session owns its statement gate and snapshot lifetime.
pub trait DirectionalQueryTask: Send {
    fn execute(self: Box<Self>, consumer: Rc<dyn QueryRowConsumer>) -> Result<(), SQLError>;
}

pub struct DirectionalQueryPlanOperator {
    schema: RowSchema,
    support: BackwardScanSupport,
    input: Option<Box<dyn DirectionalQueryTask>>,
    worker: Option<DirectionalPlanWorker>,
}

impl DirectionalQueryPlanOperator {
    pub fn new(
        input: Box<dyn DirectionalQueryTask>,
        support: BackwardScanSupport,
        schema: RowSchema,
    ) -> Self {
        Self {
            schema,
            support,
            input: Some(input),
            worker: None,
        }
    }

    fn start_worker(&mut self, direction: PhysicalScanDirection) -> ExecResult<()> {
        let input = self
            .input
            .take()
            .ok_or_else(|| ExecError::Other("directional query branch is closed".into()))?;
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let schema = self.schema.clone();
        let join = std::thread::spawn({
            move || {
                let consumer = Rc::new(DirectionalPlanRowConsumer {
                    requests: request_rx,
                    responses: response_tx.clone(),
                    direction: Cell::new(direction),
                    closed: Cell::new(false),
                    schema,
                });
                let result = input.execute(consumer.clone());
                if let Err(error) = result {
                    if !consumer.closed.get() {
                        let _ = response_tx.send(DirectionalPlanResponse::Error(error));
                    }
                } else if !consumer.closed.get() {
                    let _ = response_tx.send(DirectionalPlanResponse::Error(SQLError::Internal(
                        "directional query branch stopped without a close request".into(),
                    )));
                }
            }
        });
        self.worker = Some(DirectionalPlanWorker {
            requests: request_tx,
            responses: response_rx,
            join: Some(join),
        });
        Ok(())
    }

    fn worker(&self) -> ExecResult<&DirectionalPlanWorker> {
        self.worker
            .as_ref()
            .ok_or_else(|| ExecError::Other("directional query branch is closed".into()))
    }

    fn request(&self, request: DirectionalPlanRequest) -> ExecResult<DirectionalPlanResponse> {
        let worker = self.worker()?;
        worker.requests.send(request).map_err(|_| {
            ExecError::Other("directional query branch stopped before a request".into())
        })?;
        worker.responses.recv().map_err(|_| {
            ExecError::Other("directional query branch stopped without a response".into())
        })
    }

    fn step(&mut self, direction: PhysicalScanDirection) -> ExecResult<DirectionalPlanResponse> {
        if self.worker.is_none() {
            self.start_worker(direction)?;
            return self.worker()?.responses.recv().map_err(|_| {
                ExecError::Other("directional query branch stopped without a response".into())
            });
        }
        self.request(DirectionalPlanRequest::Step(direction))
    }
}

impl PhysicalOperator for DirectionalQueryPlanOperator {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        self.support
    }

    fn open(&mut self) -> ExecResult<()> {
        if self.input.is_some() || self.worker.is_some() {
            Ok(())
        } else {
            Err(ExecError::Other(
                "directional query branch is closed".into(),
            ))
        }
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.next_direction(PhysicalScanDirection::Forward)
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        match self.step(direction)? {
            DirectionalPlanResponse::Row(row) => Ok(Some(Batch::from_physical_rows(
                self.schema.clone(),
                vec![row],
            ))),
            DirectionalPlanResponse::Eof => Ok(None),
            DirectionalPlanResponse::Error(error) => Err(ExecError::SQL(error)),
            DirectionalPlanResponse::Rewound => Err(ExecError::Other(
                "directional query branch returned an invalid row response".into(),
            )),
        }
    }

    fn rewind(&mut self) -> ExecResult<()> {
        if self.input.is_some() {
            return Ok(());
        }
        match self.request(DirectionalPlanRequest::Rewind)? {
            DirectionalPlanResponse::Rewound => Ok(()),
            DirectionalPlanResponse::Error(error) => Err(ExecError::SQL(error)),
            DirectionalPlanResponse::Row(_) | DirectionalPlanResponse::Eof => {
                Err(ExecError::Other(
                    "directional query branch returned an invalid rewind response".into(),
                ))
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.worker = None;
        self.input = None;
        Ok(())
    }
}
