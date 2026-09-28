//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[tokio::test]
async fn invalid_options_and_channels_fail_before_an_http_request() {
    let server = Server::new().await;
    let engine = server.engine();
    let mut invalid = options();
    invalid.max_channels = 0;
    assert_eq!(
        engine
            .subscribe_notifications(&["jobs"], invalid)
            .await
            .unwrap_err()
            .kind(),
        NotificationFailureKind::InvalidRequest
    );
    let mut invalid = options();
    invalid.max_queued_bytes = 1;
    assert_eq!(
        engine
            .subscribe_notifications(&["jobs"], invalid)
            .await
            .unwrap_err()
            .kind(),
        NotificationFailureKind::Capacity
    );
    assert_eq!(
        engine
            .subscribe_notifications(&["jobs", "jobs"], options())
            .await
            .unwrap_err()
            .kind(),
        NotificationFailureKind::InvalidRequest
    );
    let cancelled = NotificationCancellation::new();
    cancelled.cancel();
    assert_eq!(
        engine
            .subscribe_notifications_with_cancellation(&["jobs"], options(), &cancelled)
            .await
            .unwrap_err()
            .kind(),
        NotificationFailureKind::Cancelled
    );
    assert!(timeout(Duration::from_millis(20), server.listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn unsupported_endpoint_is_explicit_and_never_falls_back_to_sql() {
    let server = Server::new().await;
    let task = start(server.engine(), options(), NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.head("404 Not Found", &[]).await;
    let error = finish_start(task).await.unwrap_err();
    assert_eq!(error.kind(), NotificationFailureKind::Unsupported);
    peer.expect_closed().await;
}

#[tokio::test]
async fn redirects_are_not_followed_and_credentials_remain_at_original_origin() {
    let server = Server::new().await;
    let target = Server::new().await;
    let task = start(server.engine(), options(), NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.head("307 Temporary Redirect", &[("location", &target.url)])
        .await;
    assert_eq!(
        finish_start(task).await.unwrap_err().kind(),
        NotificationFailureKind::Protocol
    );
    peer.expect_closed().await;
    assert!(timeout(Duration::from_millis(20), target.listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn response_headers_cannot_weaken_identity_encoding_or_cache_contract() {
    for bad in 0..6 {
        let server = Server::new().await;
        let task = start(server.engine(), options(), NotificationCancellation::new());
        let mut peer = server.accept().await;
        let mut headers = vec![
            ("content-type", "text/event-stream; charset=utf-8"),
            ("cache-control", "no-store, no-transform"),
            ("x-request-id", REQUEST),
        ];
        match bad {
            0 => headers[0].1 = "text/event-stream; charset=latin1",
            1 => headers[0].1 = "text/event-stream; charset=\"utf-8",
            2 => headers[1].1 = "no-cache",
            3 => headers.push(("content-encoding", "gzip")),
            4 => headers.push(("x-request-id", "another_request")),
            _ => headers[2].1 = "invalid request",
        }
        peer.head("200 OK", &headers).await;
        assert_eq!(
            finish_start(task).await.unwrap_err().kind(),
            NotificationFailureKind::Protocol
        );
        peer.expect_closed().await;
    }
}

#[tokio::test]
async fn invalid_ready_cannot_return_a_subscription() {
    for invalid in [
        ready(REQUEST, EPOCH).replace(
            "\"accepted_channel_count\":2",
            "\"accepted_channel_count\":1",
        ),
        ready(REQUEST, EPOCH).replace(REQUEST, "mismatch"),
        ready(REQUEST, EPOCH).replace(
            "\"idle_timeout_ms\":\"10000\"",
            "\"idle_timeout_ms\":\"30000\"",
        ),
        notification(REQUEST, EPOCH, 1),
    ] {
        let server = Server::new().await;
        let task = start(server.engine(), options(), NotificationCancellation::new());
        let mut peer = server.accept().await;
        peer.accepted(REQUEST).await;
        peer.chunk(invalid.as_bytes()).await;
        assert_eq!(
            finish_start(task).await.unwrap_err().kind(),
            NotificationFailureKind::Protocol
        );
        peer.expect_closed().await;
    }
}

#[tokio::test]
async fn authentication_and_private_server_diagnostics_are_terminal_and_redacted() {
    for (status, code, kind) in [
        (
            "401 Unauthorized",
            "NOTIFICATION_AUTHENTICATION",
            NotificationFailureKind::Authentication,
        ),
        (
            "403 Forbidden",
            "NOTIFICATION_AUTHORITY_REVOKED",
            NotificationFailureKind::AuthorityRevoked,
        ),
    ] {
        let server = Server::new().await;
        let mut options = options();
        options.retry = Some(retry());
        let task = start(server.engine(), options, NotificationCancellation::new());
        let mut peer = server.accept().await;
        peer.error(status, code, None).await;
        let error = finish_start(task).await.unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.server_code(), Some(code));
        assert_eq!(error.server_message(), Some("PRIVATE_SERVER_MESSAGE"));
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
        peer.expect_closed().await;
    }
}

#[tokio::test]
async fn request_head_and_missing_ready_share_the_readiness_budget() {
    for send_head in [false, true] {
        let server = Server::new().await;
        let mut options = options();
        options.connect_timeout = Duration::from_millis(100);
        options.ready_timeout = Duration::from_millis(200);
        let task = start(server.engine(), options, NotificationCancellation::new());
        let mut peer = server.accept().await;
        if send_head {
            peer.accepted(REQUEST).await;
            peer.chunk(b": not ready\n\n").await;
        }
        let error = finish_start(task).await.unwrap_err();
        assert_eq!(
            error.timeout_stage(),
            Some(NotificationTimeoutStage::Readiness)
        );
        peer.expect_closed().await;
    }
}

#[tokio::test]
async fn tls_connection_wait_has_its_own_timeout_and_cancellation() {
    for cancel in [false, true] {
        let server = Server::new().await;
        let engine = HttpEngine::new(
            &server.url.replace("http:", "https:"),
            SecretString::from(TOKEN),
        )
        .unwrap();
        let cancellation = NotificationCancellation::new();
        let mut options = options();
        options.connect_timeout = if cancel {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(200)
        };
        let task = start(engine, options, cancellation.clone());
        let mut stream = server.socket().await;
        let mut hello = [0; 1_024];
        assert!(
            timeout(WAIT, stream.read(&mut hello))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        if cancel {
            cancellation.cancel();
        }
        let error = finish_start(task).await.unwrap_err();
        if cancel {
            assert_eq!(error.kind(), NotificationFailureKind::Cancelled);
        } else {
            assert_eq!(
                error.timeout_stage(),
                Some(NotificationTimeoutStage::Connection)
            );
        }
        let mut peer = Peer { stream };
        peer.expect_closed().await;
    }
}

#[tokio::test]
async fn reuse_of_the_previous_epoch_is_terminal_after_the_visible_gap() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    let mut replacement = server.accept().await;
    replacement.accepted("request_2").await;
    replacement
        .chunk(ready("request_2", EPOCH).as_bytes())
        .await;
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::Protocol
    );
    replacement.expect_closed().await;
    subscription.close().await.unwrap();
}
