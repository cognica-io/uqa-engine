//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[tokio::test]
async fn creation_waits_for_ready_and_preserves_queued_values_before_terminal() {
    let server = Server::new().await;
    let task = start(server.engine(), options(), NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.accepted(REQUEST).await;
    peer.chunk(b": before ready\r\n\r\n").await;
    assert!(!task.is_finished());
    peer.chunk(
        format!(
            "{}{}{}",
            ready(REQUEST, EPOCH),
            notification(REQUEST, EPOCH, 1),
            closed(REQUEST, EPOCH)
        )
        .as_bytes(),
    )
    .await;
    let mut subscription = finish_start(task).await.unwrap();
    assert_eq!(
        subscription
            .initial_ready()
            .identity
            .request_id
            .as_ref()
            .unwrap()
            .as_str(),
        REQUEST
    );
    assert_eq!(subscription.identity().epoch.to_string(), EPOCH);
    assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::ServerDraining
    );
    subscription.close().await.unwrap();
    subscription.close().await.unwrap();
    assert!(next(&mut subscription).await.unwrap().is_none());
    peer.expect_closed().await;
}

#[tokio::test]
async fn lost_stream_exposes_old_gap_then_new_ready_before_replacement_values() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, mut first, mut subscription) = subscribe(options).await;
    first
        .chunk(notification(REQUEST, EPOCH, 1).as_bytes())
        .await;
    assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
    drop(first);
    let gap = next(&mut subscription).await.unwrap().unwrap();
    assert!(
        matches!(gap, NotificationEvent::ResyncRequired { identity, cause: NotificationFailureKind::Transport } if identity.epoch.to_string() == EPOCH)
    );
    assert_eq!(subscription.identity().epoch.to_string(), EPOCH);
    let mut second = server.accept().await;
    second.accepted("request_2").await;
    second
        .chunk(
            format!(
                "{}{}",
                ready("request_2", SECOND_EPOCH),
                notification("request_2", SECOND_EPOCH, 1)
            )
            .as_bytes(),
        )
        .await;
    let reconnect = next(&mut subscription).await.unwrap().unwrap();
    assert!(
        matches!(reconnect, NotificationEvent::Reconnected { identity } if identity.epoch.to_string() == SECOND_EPOCH && identity.request_id.as_ref().unwrap().as_str() == "request_2")
    );
    assert_eq!(subscription.identity().epoch.to_string(), SECOND_EPOCH);
    assert_notification(
        next(&mut subscription).await.unwrap().unwrap(),
        SECOND_EPOCH,
        1,
    );
    subscription.close().await.unwrap();
    second.expect_closed().await;
}

#[tokio::test]
async fn failed_replacements_share_one_episode_and_preserve_original_failure() {
    let mut options = options();
    options.retry = Some(retry());
    let (server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    for _ in 0..2 {
        let mut peer = server.accept().await;
        peer.error(
            "503 Service Unavailable",
            "NOTIFICATION_SOURCE_UNAVAILABLE",
            Some("0"),
        )
        .await;
        peer.expect_closed().await;
    }
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.reconnect_attempts(), Some(2));
    assert_eq!(
        error.original_failure().unwrap().kind(),
        NotificationFailureKind::Transport
    );
    assert!(error
        .original_failure()
        .unwrap()
        .transport_error()
        .unwrap()
        .url()
        .is_none());
    assert_eq!(
        error.last_attempt_failure().unwrap().http_status(),
        Some(503)
    );
    assert_eq!(
        error.last_attempt_failure().unwrap().server_message(),
        Some("PRIVATE_SERVER_MESSAGE")
    );
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    subscription.close().await.unwrap();
}

#[tokio::test]
async fn explicit_cancellation_stops_registration_and_releases_its_socket() {
    let server = Server::new().await;
    let cancellation = NotificationCancellation::new();
    let task = start(server.engine(), options(), cancellation.clone());
    let mut peer = server.accept().await;
    peer.accepted(REQUEST).await;
    cancellation.cancel();
    assert_eq!(
        finish_start(task).await.unwrap_err().kind(),
        NotificationFailureKind::Cancelled
    );
    peer.expect_closed().await;
}

#[tokio::test]
async fn abandoning_creation_aborts_the_same_owned_connection() {
    let server = Server::new().await;
    let task = start(server.engine(), options(), NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.accepted(REQUEST).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    peer.expect_closed().await;
}

#[tokio::test]
async fn dropping_a_ready_handle_closes_its_transport() {
    let (_server, mut peer, subscription) = subscribe(options()).await;
    let cancellation = subscription.cancellation();
    drop(subscription);
    assert!(cancellation.is_cancelled());
    peer.expect_closed().await;
}

#[tokio::test]
async fn cancelling_the_close_future_retains_ownership_until_close_is_repeated() {
    use std::{future::Future, task::Poll};
    let (_server, mut peer, mut subscription) = subscribe(options()).await;
    let mut closing = Box::pin(subscription.close());
    std::future::poll_fn(|context| {
        assert!(closing.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(closing);
    subscription.close().await.unwrap();
    assert!(next(&mut subscription).await.unwrap().is_none());
    peer.expect_closed().await;
}

#[tokio::test]
async fn a_receive_wait_timeout_keeps_the_subscription_alive() {
    let (_server, mut peer, mut subscription) = subscribe(options()).await;
    assert!(
        timeout(Duration::from_millis(20), subscription.next_event())
            .await
            .is_err()
    );
    assert!(!subscription.cancellation().is_cancelled());
    peer.chunk(notification(REQUEST, EPOCH, 1).as_bytes()).await;
    assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
    let cancellation = subscription.cancellation();
    cancellation.cancel();
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::Cancelled
    );
    subscription.close().await.unwrap();
    peer.expect_closed().await;
}

#[tokio::test]
async fn cancellation_interrupts_backoff_without_leaving_a_retry_task() {
    let mut options = options();
    options.retry = Some(NotificationRetryOptions {
        initial_backoff: Duration::from_secs(2),
        max_backoff: Duration::from_secs(2),
        episode_timeout: Duration::from_secs(5),
        ..retry()
    });
    let (_server, first, mut subscription) = subscribe(options).await;
    drop(first);
    assert!(matches!(
        next(&mut subscription).await.unwrap(),
        Some(NotificationEvent::ResyncRequired { .. })
    ));
    subscription.cancellation().cancel();
    assert_eq!(
        next(&mut subscription).await.unwrap_err().kind(),
        NotificationFailureKind::Cancelled
    );
    timeout(WAIT, subscription.close()).await.unwrap().unwrap();
}

#[tokio::test]
async fn queue_count_and_actual_string_capacity_overflow_are_visible() {
    for byte_limited in [false, true] {
        let mut options = options();
        if byte_limited {
            options.max_queued_events = 8;
            options.max_queued_bytes = 8 * size_of::<NotificationEvent>() + 8;
        } else {
            options.max_queued_events = 1;
        }
        let (_server, mut peer, mut subscription) = subscribe(options).await;
        peer.chunk(
            format!(
                "{}{}",
                notification(REQUEST, EPOCH, 1),
                notification(REQUEST, EPOCH, 2)
            )
            .as_bytes(),
        )
        .await;
        peer.expect_closed().await;
        if !byte_limited {
            assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
        }
        assert_eq!(
            next(&mut subscription).await.unwrap_err().kind(),
            NotificationFailureKind::Backpressure
        );
        subscription.close().await.unwrap();
    }
}

#[tokio::test]
async fn healthy_heartbeats_outlive_the_attempt_ready_deadline() {
    let mut options = options();
    options.connect_timeout = Duration::from_millis(100);
    options.ready_timeout = Duration::from_millis(200);
    let (_server, mut peer, mut subscription) = subscribe(options).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    peer.chunk(b": heartbeat\n\n").await;
    peer.chunk(notification(REQUEST, EPOCH, 1).as_bytes()).await;
    assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
    subscription.close().await.unwrap();
    peer.expect_closed().await;
}

#[tokio::test]
async fn silence_has_an_idle_deadline_distinct_from_receive_wait_and_ready() {
    let server = Server::new().await;
    let task = start(server.engine(), options(), NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.accepted(REQUEST).await;
    let frame = ready(REQUEST, EPOCH)
        .replace(
            "\"heartbeat_interval_ms\":\"100\"",
            "\"heartbeat_interval_ms\":\"10\"",
        )
        .replace(
            "\"timing_margin_ms\":\"100\"",
            "\"timing_margin_ms\":\"10\"",
        )
        .replace(
            "\"idle_timeout_ms\":\"10000\"",
            "\"idle_timeout_ms\":\"100\"",
        );
    peer.chunk(frame.as_bytes()).await;
    let mut subscription = finish_start(task).await.unwrap();
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.timeout_stage(), Some(NotificationTimeoutStage::Idle));
    peer.expect_closed().await;
    subscription.close().await.unwrap();
}
