//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mod lifecycle;
mod privacy;
mod recovery;
mod validation;

use serde_json::{json, Value};
use std::{net::Ipv4Addr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use uqa_client::{
    notifications::{
        HttpNotificationError, HttpNotificationOptions, HttpNotificationSubscription,
        NotificationCancellation, NotificationRetryOptions, NotificationTimeoutStage,
    },
    HttpEngine, SecretString,
};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind};

const TOKEN: &str = "PRIVATE_NOTIFICATION_CREDENTIAL";
const REQUEST: &str = "request_1";
const EPOCH: &str = "7fb52b7f-bdca-4db2-9ee0-490f99857201";
const SECOND_EPOCH: &str = "7fb52b7f-bdca-4db2-9ee0-490f99857202";
const WAIT: Duration = Duration::from_secs(5);

fn options() -> HttpNotificationOptions {
    HttpNotificationOptions {
        max_channels: 2,
        max_queued_events: 32,
        max_queued_bytes: 65_536,
        max_transport_chunk_bytes: 1_048_576,
        connect_timeout: Duration::from_secs(1),
        ready_timeout: Duration::from_secs(2),
        max_idle_timeout: Duration::from_secs(20),
        retry: None,
    }
}

fn retry() -> NotificationRetryOptions {
    NotificationRetryOptions {
        max_attempts: 2,
        episode_timeout: Duration::from_secs(2),
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(40),
        max_retry_after: Duration::from_secs(1),
    }
}

fn fixture(name: &str) -> String {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/notifications-v1.json")).unwrap();
    fixture[name].as_str().unwrap().to_owned()
}

fn ready(request: &str, epoch: &str) -> String {
    fixture("ready")
        .replace(REQUEST, request)
        .replace(EPOCH, epoch)
        .replace(
            "\"idle_timeout_ms\":\"401\"",
            "\"idle_timeout_ms\":\"10000\"",
        )
}

fn notification(request: &str, epoch: &str, sequence: u64) -> String {
    fixture("notification")
        .replace(REQUEST, request)
        .replace(EPOCH, epoch)
        .replace(
            "\"sequence\":\"1\"",
            &format!("\"sequence\":\"{sequence}\""),
        )
}

fn closed(request: &str, epoch: &str) -> String {
    fixture("closed")
        .replace(REQUEST, request)
        .replace(EPOCH, epoch)
}

struct Server {
    listener: TcpListener,
    url: String,
}

impl Server {
    async fn new() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        Self { listener, url }
    }
    fn engine(&self) -> HttpEngine {
        HttpEngine::new(&self.url, SecretString::from(TOKEN)).unwrap()
    }
    async fn socket(&self) -> TcpStream {
        timeout(WAIT, self.listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0
    }
    async fn accept(&self) -> Peer {
        let mut stream = self.socket().await;
        let mut bytes = Vec::new();
        let (head, body) = timeout(WAIT, async {
            let mut scratch = [0_u8; 8_192];
            loop {
                let count = stream.read(&mut scratch).await.unwrap();
                assert_ne!(count, 0, "request closed before admission");
                bytes.extend_from_slice(&scratch[..count]);
                assert!(bytes.len() < 128 * 1_024);
                if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let head = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break (
                            head.to_owned(),
                            serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                                .unwrap(),
                        );
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(head.starts_with("POST /v1/notifications/subscribe HTTP/1.1\r\n"));
        assert!(!head.to_ascii_lowercase().contains("last-event-id:"));
        assert!(!head.contains("jobs") && !head.contains("작업"));
        let headers: Vec<_> = head
            .lines()
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(key, value)| (key.to_ascii_lowercase(), value.trim()))
            })
            .collect();
        assert!(headers.contains(&("authorization".into(), format!("Bearer {TOKEN}").as_str())));
        assert!(headers.contains(&("accept".into(), "text/event-stream")));
        assert!(headers.contains(&("accept-encoding".into(), "identity")));
        assert_eq!(
            body,
            json!({"protocol_version":1,"channels":["jobs","작업"]})
        );
        Peer { stream }
    }
}

struct Peer {
    stream: TcpStream,
}

impl Peer {
    async fn head(&mut self, status: &str, headers: &[(&str, &str)]) {
        use std::fmt::Write as _;
        let mut text =
            format!("HTTP/1.1 {status}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n");
        for (key, value) in headers {
            write!(&mut text, "{key}: {value}\r\n").unwrap();
        }
        text.push_str("\r\n");
        self.stream.write_all(text.as_bytes()).await.unwrap();
    }
    async fn accepted(&mut self, request: &str) {
        self.head(
            "200 OK",
            &[
                ("content-type", "text/event-stream; charset=utf-8"),
                ("cache-control", "no-store, no-transform"),
                ("x-request-id", request),
            ],
        )
        .await;
    }
    async fn chunk(&mut self, bytes: &[u8]) {
        self.stream
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
            .await
            .unwrap();
        self.stream.write_all(bytes).await.unwrap();
        self.stream.write_all(b"\r\n").await.unwrap();
    }
    async fn end(&mut self) {
        self.stream.write_all(b"0\r\n\r\n").await.unwrap();
    }
    async fn expect_closed(&mut self) {
        let mut byte = [0];
        let result = timeout(WAIT, self.stream.read(&mut byte))
            .await
            .expect("client retained a closed subscription socket");
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|error| matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ))
        );
    }
    async fn error(&mut self, status: &str, code: &str, retry_after: Option<&str>) {
        let mut headers = vec![
            ("content-type", "application/json"),
            ("x-request-id", REQUEST),
        ];
        if let Some(value) = retry_after {
            headers.push(("retry-after", value));
        }
        self.head(status, &headers).await;
        self.chunk(
            json!({"request_id":REQUEST,"error":{"code":code,"message":"PRIVATE_SERVER_MESSAGE"}})
                .to_string()
                .as_bytes(),
        )
        .await;
        self.end().await;
    }
}

fn start(
    engine: HttpEngine,
    options: HttpNotificationOptions,
    cancellation: NotificationCancellation,
) -> JoinHandle<Result<HttpNotificationSubscription, HttpNotificationError>> {
    tokio::spawn(async move {
        engine
            .subscribe_notifications_with_cancellation(&["jobs", "작업"], options, &cancellation)
            .await
    })
}

async fn finish_start(
    task: JoinHandle<Result<HttpNotificationSubscription, HttpNotificationError>>,
) -> Result<HttpNotificationSubscription, HttpNotificationError> {
    timeout(WAIT, task).await.unwrap().unwrap()
}

async fn subscribe(
    options: HttpNotificationOptions,
) -> (Server, Peer, HttpNotificationSubscription) {
    let server = Server::new().await;
    let task = start(server.engine(), options, NotificationCancellation::new());
    let mut peer = server.accept().await;
    peer.accepted(REQUEST).await;
    peer.chunk(ready(REQUEST, EPOCH).as_bytes()).await;
    let subscription = finish_start(task).await.unwrap();
    (server, peer, subscription)
}

async fn next(
    subscription: &mut HttpNotificationSubscription,
) -> Result<Option<NotificationEvent>, HttpNotificationError> {
    timeout(WAIT, subscription.next_event()).await.unwrap()
}

fn assert_notification(event: NotificationEvent, epoch: &str, sequence: u64) {
    let NotificationEvent::Notification {
        identity,
        sequence: actual,
        notification,
    } = event
    else {
        panic!("notification expected")
    };
    assert_eq!(identity.epoch.to_string(), epoch);
    assert_eq!(actual, sequence);
    assert_eq!(notification.channel, "작업");
    assert_eq!(notification.process_id, i32::MIN);
    assert_eq!(notification.payload, fixture("expected_payload"));
}
