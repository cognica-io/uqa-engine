//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[tokio::test]
async fn malformed_stream_diagnostic_preserves_the_original_terminal_error() {
    for diagnostic in [
        json!({"category":"private SQL value"}),
        json!({"category":"syntax", "position":0}),
        json!(null),
    ] {
        let router = Router::new().route("/v1/sql/stream", post(move || { let diagnostic = diagnostic.clone(); async move {
            ([("x-request-id", REQUEST_ID), ("content-type", "application/x-ndjson")], format!("{}\n", json!({
                "type":"error", "code":"SQL_EXECUTION_FAILED", "message":"redacted", "request_id":REQUEST_ID, "diagnostic":diagnostic
            })))
        }}));
        let (url, server) = spawn_server(router).await;
        let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
        let mut stream = engine.sql_stream("SELECT 1", &[]).await.unwrap();
        let frame = stream.next_frame().await.unwrap().unwrap();
        assert!(!format!("{frame:?}").contains("private SQL value"));
        assert!(
            matches!(frame, SQLStreamFrame::Error {diagnostic: None, ref code, ..} if code == "SQL_EXECUTION_FAILED")
        );
        assert!(stream.next_frame().await.unwrap().is_none());
        server.abort();
    }
}

#[tokio::test]
async fn preserves_safe_diagnostics_for_sql_batch_and_stream() {
    let diagnostic = json!({"sqlstate": "42703", "category": "undefined_column", "statement_index": 1, "position": 8});
    let response = json!({"error": {"code": "SQL_EXECUTION_FAILED", "message": "private SQL value", "diagnostic": diagnostic}, "request_id": REQUEST_ID});
    let frame = json!({"type": "error", "code": "SQL_EXECUTION_FAILED", "message": "redacted", "diagnostic": diagnostic, "request_id": REQUEST_ID});
    let router = Router::new()
        .route(
            "/v1/sql",
            post({
                let response = response.clone();
                move || {
                    let response = response.clone();
                    async move {
                        (
                            StatusCode::BAD_REQUEST,
                            [("x-request-id", REQUEST_ID)],
                            Json(response),
                        )
                    }
                }
            }),
        )
        .route(
            "/v1/sql/batch",
            post(move || {
                let response = response.clone();
                async move {
                    (
                        StatusCode::BAD_REQUEST,
                        [("x-request-id", REQUEST_ID)],
                        Json(response),
                    )
                }
            }),
        )
        .route(
            "/v1/sql/stream",
            post(move || {
                let frame = frame.clone();
                async move {
                    (
                        [
                            ("x-request-id", REQUEST_ID),
                            ("content-type", "application/x-ndjson"),
                        ],
                        format!("{frame}\n"),
                    )
                }
            }),
        );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    for error in [
        engine.sql("SELECT 1", &[]).await.unwrap_err(),
        engine.sql_batch(&[("SELECT 1", &[])]).await.unwrap_err(),
    ] {
        assert!(!format!("{error:?} {error}").contains("private SQL value"));
        assert!(error.to_string().contains("batch statement 2"));
        let HttpEngineError::Server {
            diagnostic: Some(value),
            ..
        } = error
        else {
            panic!("missing diagnostic")
        };
        assert_eq!(serde_json::to_value(&value).unwrap(), diagnostic);
    }
    let mut stream = engine.sql_stream("SELECT 1", &[]).await.unwrap();
    let Some(SQLStreamFrame::Error {
        diagnostic: Some(value),
        ..
    }) = stream.next_frame().await.unwrap()
    else {
        panic!("missing stream diagnostic")
    };
    assert_eq!(serde_json::to_value(value).unwrap(), diagnostic);
    assert!(stream.next_frame().await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn invalid_diagnostic_cannot_echo_remote_text_or_replace_legacy_error() {
    for diagnostic in [
        json!({"category": "private SQL value", "sqlstate": "42703"}),
        json!({"category": "syntax", "sqlstate": "private SQL value"}),
        json!({"category": "syntax", "position": 0}),
        json!({"category": "syntax", "statement_index": -1}),
        json!({"category": "syntax", "hint": "private SQL value"}),
    ] {
        let router = Router::new().route("/v1/sql", post(move || {let diagnostic = diagnostic.clone(); async move {
            (StatusCode::BAD_REQUEST, [("x-request-id", REQUEST_ID)], Json(json!({
                "error": {"code": "SQL_EXECUTION_FAILED", "message": "private SQL value", "diagnostic": diagnostic}, "request_id": REQUEST_ID
            })))
        }}));
        let (url, server) = spawn_server(router).await;
        let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
        let error = engine.sql("SELECT 1", &[]).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private SQL value"));
        assert!(matches!(
            error,
            HttpEngineError::Server {
                diagnostic: None,
                ..
            }
        ));
        server.abort();
    }
}
