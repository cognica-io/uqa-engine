//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[tokio::test]
async fn server_draining_reconnects_and_a_new_ready_ends_the_previous_episode() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, mut first, mut subscription) = subscribe(options).await;
    first.chunk(closed(REQUEST, EPOCH).as_bytes()).await;
    let gap = next(&mut subscription).await.unwrap().unwrap();
    assert!(matches!(
        gap,
        NotificationEvent::ResyncRequired {
            cause: NotificationFailureKind::ServerDraining,
            ..
        }
    ));
    first.expect_closed().await;
    let mut second = server.accept().await;
    second.accepted("request_2").await;
    second
        .chunk(ready("request_2", SECOND_EPOCH).as_bytes())
        .await;
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::Reconnected { .. })
    ));
    let revoked = format!("event: error\ndata: {{\"request_id\":\"request_2\",\"stream_id\":\"{SECOND_EPOCH}\",\"code\":\"NOTIFICATION_AUTHORITY_REVOKED\",\"retryable\":true}}\n\n");
    second
        .chunk(format!("{}{revoked}", notification("request_2", SECOND_EPOCH, 1)).as_bytes())
        .await;
    second.expect_closed().await;
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.kind(), NotificationFailureKind::AuthorityRevoked);
    assert!(error.original_failure().is_none());
    assert!(error.server_failure().unwrap().retryable);
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_deadline_contains_a_stalled_replacement_ready() {
    let mut options = options();
    options.retry = Some(NotificationRetryOptions {
        max_attempts: 10,
        episode_timeout: Duration::from_millis(200),
        ..retry()
    });
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    let mut second = server.accept().await;
    second.accepted("request_2").await;
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.reconnect_attempts(), Some(1));
    assert_eq!(
        error.last_attempt_failure().unwrap().timeout_stage(),
        Some(NotificationTimeoutStage::Reconnect)
    );
    assert_eq!(
        error.original_failure().unwrap().kind(),
        NotificationFailureKind::Transport
    );
    second.expect_closed().await;
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn server_retry_guidance_cannot_extend_the_reconnect_episode() {
    let mut options = options();
    options.retry = Some(NotificationRetryOptions {
        episode_timeout: Duration::from_millis(400),
        ..retry()
    });
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    let mut second = server.accept().await;
    second
        .error("429 Too Many Requests", "NOTIFICATION_CAPACITY", Some("1"))
        .await;
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.reconnect_attempts(), Some(1));
    assert_eq!(
        error.last_attempt_failure().unwrap().timeout_stage(),
        Some(NotificationTimeoutStage::Reconnect)
    );
    second.expect_closed().await;
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn http_date_retry_guidance_is_supported_without_replaying_sql() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    let mut second = server.accept().await;
    second
        .error(
            "503 Service Unavailable",
            "NOTIFICATION_SOURCE_UNAVAILABLE",
            Some("Sun, 06 Nov 1994 08:49:37 GMT"),
        )
        .await;
    second.expect_closed().await;
    let mut third = server.accept().await;
    third.accepted("request_3").await;
    third
        .chunk(ready("request_3", SECOND_EPOCH).as_bytes())
        .await;
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::Reconnected { .. })
    ));
    subscription.close().await.unwrap();
    third.expect_closed().await;
}

#[tokio::test]
async fn cancellation_of_replacement_registration_joins_the_actual_attempt() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    let mut second = server.accept().await;
    second.accepted("request_2").await;
    subscription.cancellation().cancel();
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::Cancelled
    );
    subscription.close().await.unwrap();
    second.expect_closed().await;
}

#[tokio::test]
async fn a_long_http_response_is_not_subject_to_the_per_event_size_limit() {
    let mut options = options();
    options.max_queued_events = 512;
    options.max_queued_bytes = 1_048_576;
    let (_server, mut peer, mut subscription) = subscribe(options).await;
    let mut input = String::new();
    for sequence in 1..=400 {
        input.push_str(&notification(REQUEST, EPOCH, sequence));
    }
    input.push_str(&closed(REQUEST, EPOCH));
    assert!(input.len() > 65_536);
    peer.chunk(input.as_bytes()).await;
    peer.expect_closed().await;
    for sequence in 1..=400 {
        assert_notification(
            next(&mut subscription).await.unwrap().unwrap(),
            EPOCH,
            sequence,
        );
    }
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::ServerDraining
    );
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn protocol_corruption_after_ready_is_terminal_even_with_retry_enabled() {
    let mut options = options();
    options.retry = Some(retry());
    let (_server, mut peer, mut subscription) = subscribe(options).await;
    peer.chunk(notification(REQUEST, EPOCH, 2).as_bytes()).await;
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.kind(), NotificationFailureKind::Protocol);
    assert!(error.reconnect_attempts().is_none());
    peer.expect_closed().await;
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn malformed_http_chunking_is_corruption_not_a_retryable_loss() {
    let mut options = options();
    options.retry = Some(retry());
    let (_server, mut peer, mut subscription) = subscribe(options).await;
    peer.stream.write_all(b"not-a-hex-chunk\r\n").await.unwrap();
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.kind(), NotificationFailureKind::Protocol);
    assert!(error.reconnect_attempts().is_none());
    assert!(error.transport_error().is_some());
    peer.expect_closed().await;
    subscription.close().await.unwrap();
}
