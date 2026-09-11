//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    query::consumer::QueryRowConsumer, OwnedPhysicalRow, PhysicalRow, PhysicalScanDirection,
    RowSchema,
};
use std::{cell::Cell, sync::mpsc};
use uqa_sql::{plan::UnifiedPlan, ColumnType};

fn channels(
    directional: bool,
) -> (
    SessionPortalRowConsumer,
    mpsc::Sender<SessionPortalWorkerRequest>,
    mpsc::Receiver<SessionPortalWorkerResponse>,
) {
    let (request_tx, requests) = mpsc::channel();
    let (responses, response_rx) = mpsc::channel();
    (
        SessionPortalRowConsumer {
            requests,
            responses,
            direction: Cell::new(PhysicalScanDirection::Forward),
            directional,
            closed: Cell::new(false),
            public_width: Cell::new(0),
        },
        request_tx,
        response_rx,
    )
}

#[test]
fn duplicate_output_names_retain_their_positional_types() {
    let (consumer, _requests, responses) = channels(false);
    let columns = vec!["value".into(), "value".into()];
    let types = vec![Some(ColumnType::Integer), Some(ColumnType::Text)];
    consumer
        .begin(
            &columns,
            &RowSchema::with_types(columns.clone(), types.clone()),
        )
        .unwrap();
    let SessionPortalWorkerResponse::Started {
        columns: actual,
        column_types,
    } = responses.recv().unwrap()
    else {
        panic!("metadata must precede rows");
    };
    assert_eq!(actual, columns);
    assert_eq!(column_types, types);
}

#[test]
fn reordered_output_names_resolve_types_from_their_schema_positions() {
    let (consumer, _requests, responses) = channels(false);
    consumer
        .begin(
            &["visible".into()],
            &RowSchema::with_types(
                vec!["internal".into(), "visible".into()],
                vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
            ),
        )
        .unwrap();
    let SessionPortalWorkerResponse::Started { column_types, .. } = responses.recv().unwrap()
    else {
        panic!("expected startup metadata");
    };
    assert_eq!(column_types, [Some(ColumnType::Text)]);
}

#[test]
fn rows_use_public_width_without_exposing_internal_values() {
    for (public_width, expected) in [
        (1, vec![Value::Int(10)]),
        (3, vec![Value::Int(10), Value::Int(20), Value::Null]),
    ] {
        let (consumer, requests, responses) = channels(false);
        let schema = RowSchema::new(vec!["a".into(), "b".into()]);
        consumer.public_width.set(public_width);
        requests.send(SessionPortalWorkerRequest::Close).unwrap();
        let control = consumer
            .consume(OwnedPhysicalRow::new(
                schema,
                PhysicalRow::from_values(vec![Value::Int(10), Value::Int(20)]),
            ))
            .unwrap();
        assert!(matches!(control, QueryConsumerControl::Stop));
        let SessionPortalWorkerResponse::Row(values) = responses.recv().unwrap() else {
            panic!("expected positional row");
        };
        assert_eq!(values, expected);
        assert!(consumer.closed.get());
    }
}

#[test]
fn directional_requests_preserve_reverse_steps_and_rewind_handshake() {
    let (consumer, requests, responses) = channels(true);
    requests
        .send(SessionPortalWorkerRequest::Step(
            PhysicalScanDirection::Backward,
        ))
        .unwrap();
    assert!(matches!(
        consumer.direction_exhausted().unwrap(),
        QueryConsumerControl::Continue
    ));
    assert!(matches!(
        responses.recv().unwrap(),
        SessionPortalWorkerResponse::Eof
    ));
    assert_eq!(consumer.scan_direction(), PhysicalScanDirection::Backward);
    requests.send(SessionPortalWorkerRequest::Rewind).unwrap();
    assert!(matches!(
        consumer.wait_for_request().unwrap(),
        QueryConsumerControl::Rewind
    ));
    requests.send(SessionPortalWorkerRequest::Close).unwrap();
    assert!(matches!(
        consumer.rewound().unwrap(),
        QueryConsumerControl::Stop
    ));
    assert!(matches!(
        responses.recv().unwrap(),
        SessionPortalWorkerResponse::Rewound
    ));
}

#[test]
fn forward_only_rewind_is_an_error_and_disconnect_stops_the_consumer() {
    let (consumer, requests, responses) = channels(false);
    requests.send(SessionPortalWorkerRequest::Rewind).unwrap();
    let error = consumer.wait_for_request().err().expect("rewind must fail");
    assert!(
        matches!(error, SQLError::Internal(message) if message == "forward-only cursor worker received a rewind request")
    );
    drop(responses);
    assert!(matches!(
        consumer.direction_exhausted().unwrap(),
        QueryConsumerControl::Stop
    ));
    assert!(consumer.closed.get());
}

#[test]
fn dropping_the_worker_closes_and_joins_its_thread() {
    let (requests, request_rx) = mpsc::channel();
    let (_response_tx, responses) = mpsc::channel();
    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = closed.clone();
    let join = std::thread::spawn(move || {
        if matches!(request_rx.recv(), Ok(SessionPortalWorkerRequest::Close)) {
            observed.store(true, std::sync::atomic::Ordering::Release);
        }
    });
    drop(SessionPortalWorker {
        requests,
        responses,
        join: Some(join),
    });
    assert!(closed.load(std::sync::atomic::Ordering::Acquire));
}

struct UnopenedQuery;

impl StatementQueryContexts<()> for UnopenedQuery {
    fn statement_scope(&self, _: Option<&str>) -> crate::query::CteScope<()> {
        panic!("a portal must wait for a step before capturing its catalog scope")
    }
    fn query_context(&self) -> crate::query::statement::context::QueryContext<'_, ()> {
        panic!("an unopened portal cannot execute a query")
    }
    fn row_lock_context(&self) -> crate::query::locking::RowLockContext<'_, ()> {
        panic!("an unopened worker cannot request row locks")
    }
    fn with_expression_context(
        &self,
        _: &crate::query::CteScope<()>,
        _: &[SQLParam],
        _: &mut crate::statement::context::queries::StatementExpressionOperation<'_>,
    ) -> Result<uqa_sql::SQLResult, SQLError> {
        panic!("an unopened worker cannot evaluate expressions")
    }
}

fn unopened_worker(directional: bool) -> Vec<SessionPortalWorkerResponse> {
    let (requests, request_rx) = mpsc::channel();
    let (response_tx, responses) = mpsc::channel();
    requests.send(SessionPortalWorkerRequest::Rewind).unwrap();
    requests.send(SessionPortalWorkerRequest::Close).unwrap();
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("expected a query");
    };
    run(
        &UnopenedQuery,
        &query,
        &[],
        directional,
        request_rx,
        response_tx,
    );
    responses.into_iter().collect()
}

#[test]
fn prestart_rewind_acknowledges_without_capturing_a_query_scope() {
    assert!(matches!(
        unopened_worker(true).as_slice(),
        [SessionPortalWorkerResponse::Rewound]
    ));
}

#[test]
fn prestart_forward_only_rewind_exits_without_capturing_a_query_scope() {
    assert!(unopened_worker(false).is_empty());
}
