//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent directional execution for one set-operation child plan.

use std::cell::Cell;
use std::rc::Rc;

use uqa_execution::{
    BackwardScanSupport, Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow,
    PhysicalScanDirection, RowSchema,
};
use uqa_planner::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::ast::FunctionBinding;
use uqa_sql::{SQLError, SQLParam};

use super::{
    builtin_returns_set, execute_query_plan_output, CteScope, Engine, QueryConsumerControl,
    QueryOutputMode, QueryRowConsumer, SetOpKind, Value,
};

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
    fn begin(
        &self,
        _engine: &Engine,
        columns: &[String],
        _schema: &RowSchema,
    ) -> Result<(), SQLError> {
        if columns.len() != self.schema.len() {
            return Err(SQLError::TypeMismatch(format!(
                "directional set-operation branch width {} does not match declared width {}",
                columns.len(),
                self.schema.len()
            )));
        }
        Ok(())
    }

    fn consume(
        &self,
        _engine: &Engine,
        row: uqa_execution::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError> {
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

    fn direction_exhausted(&self, _engine: &Engine) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(DirectionalPlanResponse::Eof)
    }

    fn rewound(&self, _engine: &Engine) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(DirectionalPlanResponse::Rewound)
    }
}

struct DirectionalPlanInput {
    engine: Engine,
    plan: QueryPlan,
    params: Vec<SQLParam>,
    ctes: CteScope,
}

pub(super) struct DirectionalQueryPlanOperator {
    schema: RowSchema,
    support: BackwardScanSupport,
    input: Option<DirectionalPlanInput>,
    worker: Option<DirectionalPlanWorker>,
}

impl DirectionalQueryPlanOperator {
    pub(super) fn new(
        engine: Engine,
        plan: QueryPlan,
        params: Vec<SQLParam>,
        ctes: CteScope,
        schema: RowSchema,
    ) -> ExecResult<Self> {
        let support = query_plan_backward_scan_support(&engine, &plan);
        Ok(Self {
            schema,
            support,
            input: Some(DirectionalPlanInput {
                engine,
                plan,
                params,
                ctes,
            }),
            worker: None,
        })
    }

    fn start_worker(&mut self, direction: PhysicalScanDirection) -> ExecResult<()> {
        let DirectionalPlanInput {
            engine,
            plan,
            params,
            mut ctes,
        } = self
            .input
            .take()
            .ok_or_else(|| ExecError::Other("directional query branch is closed".into()))?;
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let schema = self.schema.clone();
        let join = std::thread::spawn({
            move || {
                let _statement_gate = engine.runtime.statement_gate.delegate_to_current_thread();
                let consumer = Rc::new(DirectionalPlanRowConsumer {
                    requests: request_rx,
                    responses: response_tx.clone(),
                    direction: Cell::new(direction),
                    closed: Cell::new(false),
                    schema,
                });
                let result = execute_query_plan_output(
                    &engine,
                    &plan,
                    &params,
                    &mut ctes,
                    QueryOutputMode::RowConsumer(consumer.clone()),
                );
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

fn query_plan_backward_scan_support(engine: &Engine, plan: &QueryPlan) -> BackwardScanSupport {
    match &plan.root {
        RelationalPlan::Values { .. } => BackwardScanSupport::Native,
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            ..
        } if matches!((*kind, *all), (SetOpKind::Union, true)) && order_by.is_empty() => {
            if query_plan_backward_scan_support(engine, left) == BackwardScanSupport::Native
                && query_plan_backward_scan_support(engine, right) == BackwardScanSupport::Native
            {
                BackwardScanSupport::Native
            } else {
                BackwardScanSupport::Unsupported
            }
        }
        RelationalPlan::SetOp { .. } => BackwardScanSupport::Unsupported,
        RelationalPlan::QueryBlock(block) => query_block_backward_scan_support(engine, block),
    }
}

fn query_block_backward_scan_support(
    engine: &Engine,
    block: &QueryBlockPlan,
) -> BackwardScanSupport {
    if block.distinct
        || !block.distinct_on.is_empty()
        || !block.locking.is_empty()
        || matches!(block.compute, ComputePlan::Window)
        || projections_may_return_set_statically(engine, block)
    {
        return BackwardScanSupport::Unsupported;
    }
    if matches!(block.compute, ComputePlan::Aggregate) {
        return if block.order_by.is_empty() {
            BackwardScanSupport::Unsupported
        } else {
            BackwardScanSupport::Native
        };
    }
    let Some(source) = block.from.as_ref() else {
        return BackwardScanSupport::Unsupported;
    };
    if !block.order_by.is_empty() {
        return BackwardScanSupport::Native;
    }
    source_backward_scan_support(engine, source)
}

fn source_backward_scan_support(engine: &Engine, source: &SourcePlan) -> BackwardScanSupport {
    match source {
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => BackwardScanSupport::Native,
        SourcePlan::Subquery { body, .. } => query_plan_backward_scan_support(engine, body),
        SourcePlan::Join { .. } => BackwardScanSupport::Unsupported,
    }
}

fn projections_may_return_set_statically(engine: &Engine, block: &QueryBlockPlan) -> bool {
    block.projections.iter().any(|projection| {
        let mut returns_set = false;
        projection.expr.visit(&mut |expression| {
            let uqa_execution::ScalarExpr::Func { name, binding, .. } = expression else {
                return;
            };
            returns_set |= function_may_return_set_statically(engine, name, binding.as_ref());
        });
        returns_set
    })
}

fn function_may_return_set_statically(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
) -> bool {
    if binding.and_then(|binding| binding.dispatch).is_some()
        || binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
    {
        return false;
    }
    let identity = name.to_ascii_lowercase();
    let builtin = crate::sql::builtin_function_dispatch_name(&identity);
    if builtin_returns_set(&builtin) || engine.has_registered_table_function(&identity) {
        return true;
    }
    if binding.is_some_and(|binding| binding.builtin) {
        return false;
    }
    let overloads = match binding {
        Some(binding) => engine.lookup_bound_sql_functions_by_binding(binding),
        None => engine
            .lookup_visible_sql_functions_for_analysis(name)
            .ok()
            .flatten(),
    };
    let Some(overloads) = overloads else {
        return false;
    };
    if let Some(binding) = binding {
        return overloads.iter().any(|function| {
            !function.def.is_procedure
                && crate::engine_user_functions::routine_signature_types(&function.def)
                    == binding.argument_types
                && function.def.returns_set()
        });
    }
    overloads
        .iter()
        .any(|function| !function.def.is_procedure && function.def.returns_set())
}
