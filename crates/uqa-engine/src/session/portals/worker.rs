//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the child Engine and delegated statement gate throughout portal execution.

use crate::{Engine, SessionPortalWorker};
use uqa_sql::{plan::QueryPlan, SQLParam};

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
        uqa_execution::statement::portal::worker::run(
            &engine,
            uqa_sql::expr::TemporalInputContext {
                transaction_clock_micros: Some(engine.transaction_timestamp_micros()),
                date_order: uqa_sql::semantics::parameters::datestyle::date_order(
                    &engine.session.setting("DateStyle"),
                ),
            },
            &query,
            &params,
            directional,
            request_rx,
            response_tx,
        );
    });
    SessionPortalWorker {
        requests: request_tx,
        responses: response_rx,
        join: Some(join),
    }
}
