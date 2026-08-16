//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::net::Ipv4Addr;

use axum::body::Body;
use axum::extract::Json;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::post;
use axum::Router;
use secrecy::SecretString;
use serde_json::{json, Value as JSONValue};
use tokio::task::JoinHandle;
use uqa_client::{HttpEngine, HttpEngineError, SQLParam, SQLStreamFrame};
use uqa_core::Value;

const TOKEN: &str = "uqa_db_http-engine-test-token";
const REQUEST_ID: &str = "qry_http_engine_test";

async fn spawn_server(router: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}/"), server)
}

fn assert_authorized(headers: &HeaderMap) {
    assert_eq!(
        headers
            .get(reqwest::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

#[tokio::test]
async fn sql_posts_typed_parameters_and_returns_engine_result() {
    let router = Router::new().route(
        "/v1/sql",
        post(
            |headers: HeaderMap, Json(body): Json<JSONValue>| async move {
                assert_authorized(&headers);
                assert_eq!(
                    body,
                    json!({
                        "sql": "SELECT $1 AS id, $2 AS embedding",
                        "params": [
                            {"type": "int64", "value": 42},
                            {"type": "vector", "value": [0.25, 0.75]}
                        ]
                    })
                );
                (
                    [("x-request-id", REQUEST_ID)],
                    Json(json!({
                        "columns": ["id", "label"],
                        "rows": [{"id": 42, "label": "remote"}],
                        "affected_rows": 0,
                        "request_id": REQUEST_ID
                    })),
                )
            },
        ),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let execution = engine
        .sql_with_metadata(
            "SELECT $1 AS id, $2 AS embedding",
            &[
                SQLParam::scalar(Value::Int(42)),
                SQLParam::vector(vec![0.25, 0.75]),
            ],
        )
        .await
        .unwrap();

    assert_eq!(execution.request_id(), REQUEST_ID);
    assert_eq!(execution.columns, ["id", "label"]);
    assert_eq!(execution.rows[0].get("id"), Some(&Value::Int(42)));
    assert_eq!(
        execution.rows[0].get("label"),
        Some(&Value::Str("remote".to_owned()))
    );
    server.abort();
}

#[tokio::test]
async fn sql_batch_uses_the_atomic_endpoint_and_preserves_request_id() {
    let router = Router::new().route(
        "/v1/sql/batch",
        post(
            |headers: HeaderMap, Json(body): Json<JSONValue>| async move {
                assert_authorized(&headers);
                assert_eq!(body["statements"].as_array().unwrap().len(), 2);
                (
                    [("x-request-id", REQUEST_ID)],
                    Json(json!({
                        "results": [
                            {"columns": [], "rows": [], "affected_rows": 1},
                            {"columns": ["count"], "rows": [{"count": 1}], "affected_rows": 0}
                        ],
                        "request_id": REQUEST_ID
                    })),
                )
            },
        ),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let id = [SQLParam::scalar(Value::Int(1))];
    let statements = [
        ("INSERT INTO items(id) VALUES ($1)", &id[..]),
        ("SELECT count(*) AS count FROM items", &[][..]),
    ];
    let execution = engine.sql_batch_with_metadata(&statements).await.unwrap();

    assert_eq!(execution.request_id(), REQUEST_ID);
    assert_eq!(execution.results().len(), 2);
    assert_eq!(execution.results()[0].affected_rows, 1);
    assert_eq!(
        execution.results()[1].rows[0].get("count"),
        Some(&Value::Int(1))
    );
    server.abort();
}

#[tokio::test]
async fn sql_stream_validates_ndjson_sequence_and_request_identity() {
    let router = Router::new().route(
        "/v1/sql/stream",
        post(|headers: HeaderMap, Json(_): Json<JSONValue>| async move {
            assert_authorized(&headers);
            assert_eq!(
                headers
                    .get(reqwest::header::ACCEPT)
                    .and_then(|value| value.to_str().ok()),
                Some("application/x-ndjson")
            );
            let body = format!(
                "{{\"type\":\"metadata\",\"columns\":[\"id\"],\"row_count\":1,\"spilled_to_disk\":false,\"request_id\":\"{REQUEST_ID}\"}}\n\
                 {{\"type\":\"row\",\"row\":{{\"id\":42}}}}\n\
                 {{\"type\":\"complete\",\"row_count\":1,\"request_id\":\"{REQUEST_ID}\"}}\n"
            );
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/x-ndjson")
                .header("x-request-id", REQUEST_ID)
                .body(Body::from(body))
                .unwrap()
        }),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let mut stream = engine.sql_stream("SELECT 42 AS id", &[]).await.unwrap();

    assert_eq!(stream.request_id(), REQUEST_ID);
    assert!(matches!(
        stream.next_frame().await.unwrap(),
        Some(SQLStreamFrame::Metadata { row_count: 1, .. })
    ));
    assert!(matches!(
        stream.next_frame().await.unwrap(),
        Some(SQLStreamFrame::Row { row }) if row.get("id") == Some(&Value::Int(42))
    ));
    assert!(matches!(
        stream.next_frame().await.unwrap(),
        Some(SQLStreamFrame::Complete { row_count: 1, .. })
    ));
    assert!(stream.next_frame().await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn sql_stream_rejects_frames_after_the_terminal_frame() {
    let router = Router::new().route(
        "/v1/sql/stream",
        post(|| async move {
            let body = format!(
                "{{\"type\":\"metadata\",\"columns\":[],\"row_count\":0,\"spilled_to_disk\":false,\"request_id\":\"{REQUEST_ID}\"}}\n\
                 {{\"type\":\"complete\",\"row_count\":0,\"request_id\":\"{REQUEST_ID}\"}}\n\
                 {{\"type\":\"row\",\"row\":{{}}}}\n"
            );
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/x-ndjson")
                .header("x-request-id", REQUEST_ID)
                .body(Body::from(body))
                .unwrap()
        }),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let mut stream = engine
        .sql_stream("SELECT 1 WHERE FALSE", &[])
        .await
        .unwrap();

    assert!(matches!(
        stream.next_frame().await.unwrap(),
        Some(SQLStreamFrame::Metadata { .. })
    ));
    assert!(matches!(
        stream.next_frame().await.unwrap(),
        Some(SQLStreamFrame::Complete { .. })
    ));
    assert!(matches!(
        stream.next_frame().await,
        Err(HttpEngineError::InvalidStreamSequence)
    ));
    server.abort();
}

#[tokio::test]
async fn server_error_keeps_diagnostics_but_redacts_customer_message() {
    let message = "customer SQL failed near private_table";
    let router = Router::new().route(
        "/v1/sql",
        post(move || async move {
            (
                StatusCode::BAD_REQUEST,
                [("x-request-id", REQUEST_ID)],
                Json(json!({
                    "error": {"code": "SQL_EXECUTION_FAILED", "message": message},
                    "request_id": REQUEST_ID
                })),
            )
        }),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let error = engine
        .sql("SELECT * FROM private_table", &[])
        .await
        .unwrap_err();

    match &error {
        HttpEngineError::Server {
            status,
            code,
            message: response_message,
            request_id,
        } => {
            assert_eq!(*status, StatusCode::BAD_REQUEST);
            assert_eq!(code, "SQL_EXECUTION_FAILED");
            assert_eq!(response_message, message);
            assert_eq!(request_id.as_deref(), Some(REQUEST_ID));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!format!("{error:?}").contains(message));
    server.abort();
}
