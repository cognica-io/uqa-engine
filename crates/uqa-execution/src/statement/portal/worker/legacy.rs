//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The original single-row channel API uses the same resumable query worker.

use super::{
    run_query, ResumableQueryRequest, ResumableQueryResponse, WorkerChannels, WorkerReply,
};
use crate::{
    query::diagnostics::InvocationRequest,
    statement::{context::queries::StatementQueryContexts, thread_context::StatementThreadContext},
    PhysicalScanDirection, RowSchema,
};
use std::sync::mpsc;
use uqa_core::Value;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

#[derive(Default)]
pub enum SessionPortalWorkerRequest {
    Step(PhysicalScanDirection, Option<InvocationRequest>),
    Rewind(Option<InvocationRequest>),
    #[default]
    Close,
}

pub enum SessionPortalWorkerResponse {
    Started { schema: RowSchema },
    Row(Vec<Value>),
    Eof,
    Rewound,
    Error(SQLError),
}

pub type SessionPortalWorker =
    WorkerChannels<SessionPortalWorkerRequest, SessionPortalWorkerResponse>;

impl From<SessionPortalWorkerRequest> for ResumableQueryRequest {
    fn from(request: SessionPortalWorkerRequest) -> Self {
        match request {
            SessionPortalWorkerRequest::Step(direction, diagnostics) => {
                Self::Step(direction, diagnostics)
            }
            SessionPortalWorkerRequest::Rewind(diagnostics) => Self::Rewind(diagnostics),
            SessionPortalWorkerRequest::Close => Self::Close,
        }
    }
}

impl WorkerReply<Vec<Value>> for SessionPortalWorkerResponse {
    fn reply(response: ResumableQueryResponse<Vec<Value>>) -> Self {
        match response {
            ResumableQueryResponse::Started { schema } => Self::Started { schema },
            ResumableQueryResponse::Row(row) => Self::Row(row),
            ResumableQueryResponse::Eof => Self::Eof,
            ResumableQueryResponse::Rewound => Self::Rewound,
            ResumableQueryResponse::Error(error) => Self::Error(error),
            ResumableQueryResponse::Batch(_) => {
                unreachable!("single-row clients cannot request a batch")
            }
        }
    }
}

pub fn run<S: Clone + Send + Sync + 'static>(
    queries: &dyn StatementQueryContexts<S>,
    thread_context: &StatementThreadContext,
    query: &QueryPlan,
    params: &[SQLParam],
    directional: bool,
    requests: mpsc::Receiver<SessionPortalWorkerRequest>,
    responses: mpsc::Sender<SessionPortalWorkerResponse>,
) {
    run_query(
        queries,
        thread_context,
        query,
        params,
        directional,
        requests,
        responses,
    );
}
