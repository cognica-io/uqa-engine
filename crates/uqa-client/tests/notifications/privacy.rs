//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Record},
    Id, Metadata, Subscriber,
};

#[derive(Default)]
struct Capture {
    text: String,
    exceeded: bool,
}
static CAPTURE: OnceLock<Mutex<Capture>> = OnceLock::new();
static LOGGER: TransportLogger = TransportLogger;
static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);

fn record(value: fmt::Arguments<'_>) {
    use std::fmt::Write;
    let mut capture = CAPTURE.get_or_init(Mutex::default).lock().unwrap();
    if capture.text.len() > 1_048_576 {
        capture.exceeded = true;
        return;
    }
    writeln!(&mut capture.text, "{value}").unwrap();
}

struct TransportLogger;
impl log::Log for TransportLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, value: &log::Record<'_>) {
        record(format_args!("{}: {}", value.target(), value.args()));
    }
    fn flush(&self) {}
}

struct Fields;
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        record(format_args!("{}={value:?}", field.name()));
    }
}

struct Traces;
impl Subscriber for Traces {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        attributes.record(&mut Fields);
        Id::from_u64(NEXT_SPAN.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &Id, record: &Record<'_>) {
        record.record(&mut Fields);
    }
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut Fields);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_transport_trace_and_log_output_exclude_subscription_content() {
    log::set_logger(&LOGGER).unwrap();
    let previous = log::max_level();
    log::set_max_level(log::LevelFilter::Trace);
    let _traces = tracing::subscriber::set_default(Traces);
    let (_server, mut peer, mut subscription) = subscribe(options()).await;
    peer.chunk(notification(REQUEST, EPOCH, 1).as_bytes()).await;
    assert_notification(next(&mut subscription).await.unwrap().unwrap(), EPOCH, 1);
    let remote = format!("event: error\ndata: {{\"request_id\":\"{REQUEST}\",\"stream_id\":\"{EPOCH}\",\"code\":\"PRIVATE_UNKNOWN_CODE\",\"retryable\":true}}\n\n");
    peer.chunk(remote.as_bytes()).await;
    let error = next(&mut subscription).await.unwrap_err();
    assert_eq!(error.kind(), NotificationFailureKind::Protocol);
    assert_eq!(error.server_code(), Some("PRIVATE_UNKNOWN_CODE"));
    assert!(!format!("{subscription:?} {error:?} {error}").contains("PRIVATE"));
    subscription.close().await.unwrap();
    peer.expect_closed().await;
    log::set_max_level(previous);
    let capture = CAPTURE.get().unwrap().lock().unwrap();
    assert!(!capture.exceeded, "trace capture exceeded its test bound");
    assert!(
        !capture.text.is_empty(),
        "TRACE/log capture did not observe the actual transport"
    );
    for secret in [
        TOKEN,
        "작업",
        "문자",
        "PRIVATE_UNKNOWN_CODE",
        "PRIVATE_SERVER_MESSAGE",
    ] {
        assert!(
            !capture.text.contains(secret),
            "private subscription content appeared in TRACE/log output"
        );
    }
}
