//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

async fn rejected_request(code: &str, limit: JSONValue, request_id: &str) -> HttpEngineError {
    let payload = json!({ "error": { "code": code, "message": "private server query",
        "limit": limit }, "request_id": request_id });
    let router = Router::new().route(
        "/v1/sql/batch",
        post(move || {
            let payload = payload.clone();
            async move {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    [("x-request-id", REQUEST_ID)],
                    Json(payload),
                )
            }
        }),
    );
    let (url, server) = spawn_server(router).await;
    let engine = HttpEngine::new(&url, SecretString::from(TOKEN)).unwrap();
    let error = engine.sql_batch(&[("SELECT 1", &[])]).await.unwrap_err();
    server.abort();
    error
}

#[tokio::test]
async fn safe_limits_preserve_counts_bounds_and_original_response_identity() {
    for (kind, code, limit, actual, lower, executing) in [
        (
            "batch_statements",
            "TOO_MANY_STATEMENTS",
            128,
            200,
            false,
            None,
        ),
        (
            "batch_statements",
            "TOO_MANY_STATEMENTS",
            128,
            400,
            false,
            None,
        ),
        ("sql_bytes", "SQL_TOO_LARGE", 128, 200, false, None),
        ("parameters", "TOO_MANY_PARAMETERS", 0, 1, false, None),
        (
            "request_body_bytes",
            "REQUEST_BODY_TOO_LARGE",
            128,
            129,
            true,
            None,
        ),
        ("result_bytes", "RESULT_TOO_LARGE", 128, 200, false, None),
        ("query_admission", "NODE_BUSY", 8, 9, true, Some(16)),
        ("authentication_admission", "NODE_BUSY", 4, 5, true, Some(4)),
    ] {
        let value = json!({ "kind": kind, "limit": limit, "actual": actual,
            "actual_is_lower_bound": lower, "executing_limit": executing });
        let error = rejected_request(code, value, REQUEST_ID).await;
        let formatted = format!("{error:?} {error}");
        assert!(!formatted.contains("private server query"));
        assert!(error.to_string().contains(&format!("{kind} limit {limit}")));
        assert!(error.to_string().contains(&format!(
            "observed {}{actual}",
            if lower { "at least " } else { "" }
        )));
        let HttpEngineError::Server {
            status,
            code: actual_code,
            request_id,
            limit: Some(value),
            ..
        } = error
        else {
            panic!("missing safe limit")
        };
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(actual_code, code);
        assert_eq!(request_id.as_deref(), Some(REQUEST_ID));
        assert_eq!(value.kind.as_str(), kind);
        assert_eq!(value.limit, limit);
        assert_eq!(value.actual, actual);
        assert_eq!(value.actual_is_lower_bound, lower);
        assert_eq!(value.executing_limit, executing);
    }
}

#[tokio::test]
async fn malformed_optional_limits_preserve_the_original_error_without_remote_text() {
    let valid = json!({"kind":"batch_statements", "limit":128, "actual":200, "actual_is_lower_bound":false});
    let mut invalid = vec![json!(null), json!([]), json!("private server query")];
    for (key, value) in [
        ("kind", json!("private server query")),
        ("kind", json!("sql_bytes")),
        ("limit", json!(-1)),
        ("limit", json!(0)),
        ("limit", json!(128.5)),
        ("limit", json!("128")),
        ("actual", json!(128)),
        ("actual", json!(9_007_199_254_740_992_u64)),
        ("actual_is_lower_bound", json!(true)),
        ("actual_is_lower_bound", json!("false")),
        ("executing_limit", json!(16)),
        ("hint", json!("private server query")),
    ] {
        let mut metadata = valid.clone();
        metadata[key] = value;
        invalid.push(metadata);
    }
    for key in ["kind", "limit", "actual", "actual_is_lower_bound"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(key);
        invalid.push(value);
    }
    for value in invalid {
        let error = rejected_request("TOO_MANY_STATEMENTS", value, REQUEST_ID).await;
        assert_eq!(
            error.to_string(),
            "UQA returned 413 Payload Too Large with code TOO_MANY_STATEMENTS"
        );
        assert!(!format!("{error:?}").contains("private server query"));
        assert!(matches!(error, HttpEngineError::Server { limit: None, .. }));
    }
    let error = rejected_request("TOO_MANY_STATEMENTS", valid, "different-request").await;
    assert!(matches!(error, HttpEngineError::ResponseRequestIdMismatch));
}
