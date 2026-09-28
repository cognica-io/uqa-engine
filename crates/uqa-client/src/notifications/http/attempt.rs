//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single original-origin HTTP attempt. No hidden transport retry, compression or SQL deadline.

use super::{
    options, HttpNotificationError as Error, HttpNotificationOptions,
    NotificationTimeoutStage as Stage,
};
use crate::notifications::{
    json, NotificationDecoder, NotificationReady, NotificationWireEvent, ProtocolError,
    SubscriptionRequest, MAX_NOTIFICATION_WIRE_BYTES,
};
use reqwest::{
    header::{HeaderMap, HeaderName},
    Client, Response, Url,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::time::{timeout_at, Instant};
use uqa_core::notifications::{NotificationFailureKind, NotificationRequestId};

pub(super) struct Connection {
    client: Client,
    endpoint: Url,
    credential: SecretString,
    body: Vec<u8>,
    pub request: Arc<SubscriptionRequest>,
    pub options: HttpNotificationOptions,
}

impl Connection {
    pub fn new(
        endpoint: Url,
        credential: SecretString,
        request: Arc<SubscriptionRequest>,
        options: HttpNotificationOptions,
    ) -> Result<Self, Error> {
        let body = request.encode().map_err(Error::protocol)?;
        let builder = Client::builder()
            .no_proxy()
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .connection_verbose(false)
            .connect_timeout(options.connect_timeout)
            .pool_max_idle_per_host(0)
            .tcp_keepalive(None)
            .user_agent(concat!("uqa-client/", env!("CARGO_PKG_VERSION")));
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(None);
        let client = builder.build().map_err(Error::transport)?;
        Ok(Self {
            client,
            endpoint,
            credential,
            body,
            request,
            options,
        })
    }

    pub async fn open(&self, episode_deadline: Option<Instant>) -> Result<Attempt, Error> {
        let mut ready_deadline = options::deadline(self.options.ready_timeout)?;
        if let Some(episode) = episode_deadline {
            ready_deadline = ready_deadline.min(episode);
        }
        let response = timeout_at(
            ready_deadline,
            self.client
                .post(self.endpoint.clone())
                .bearer_auth(self.credential.expose_secret())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .body(self.body.clone())
                .send(),
        )
        .await
        .map_err(|_| Error::timeout(Stage::Readiness))?
        .map_err(Error::transport)?;
        if response.status().as_u16() != 200 {
            return Err(
                timeout_at(ready_deadline, response_error(response, &self.options))
                    .await
                    .map_err(|_| Error::timeout(Stage::Readiness))?,
            );
        }
        let request_id = validate_headers(response.headers())?;
        let decoder = NotificationDecoder::new(
            Arc::clone(&self.request),
            request_id,
            self.options.timer_limits(),
        )
        .map_err(Error::protocol)?;
        Ok(Attempt {
            response,
            decoder,
            ready_deadline,
            idle_deadline: None,
            ended: false,
        })
    }
}

pub(super) struct Attempt {
    pub response: Response,
    pub decoder: NotificationDecoder,
    ready_deadline: Instant,
    idle_deadline: Option<Instant>,
    ended: bool,
}

impl Attempt {
    pub fn deadline(&self) -> Instant {
        self.idle_deadline.unwrap_or(self.ready_deadline)
    }
    pub fn timeout(&self) -> Error {
        Error::timeout(if self.idle_deadline.is_some() {
            Stage::Idle
        } else {
            Stage::Readiness
        })
    }
    pub fn received_bytes(&mut self) -> Result<(), Error> {
        if let Some(ready) = self.decoder.ready() {
            self.idle_deadline = Some(options::deadline(Duration::from_millis(
                ready.timing.idle_timeout_ms(),
            ))?);
        }
        Ok(())
    }
    pub fn admit_ready(&mut self, ready: &NotificationReady) -> Result<(), Error> {
        self.idle_deadline = Some(options::deadline(Duration::from_millis(
            ready.timing.idle_timeout_ms(),
        ))?);
        Ok(())
    }
    pub fn finish(&mut self) -> Result<Option<NotificationWireEvent>, Error> {
        self.ended = true;
        self.decoder.finish().map_err(Error::protocol)
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
}

fn single<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Result<Option<&'a str>, Error> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| Error::protocol(ProtocolError::InvalidFields))?;
    if values.next().is_some() {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    Ok(value)
}

fn request_id(headers: &HeaderMap) -> Result<NotificationRequestId, Error> {
    let value = single(headers, &HeaderName::from_static("x-request-id"))?
        .ok_or_else(|| Error::protocol(ProtocolError::Identity))?;
    NotificationRequestId::new(value).map_err(|_| Error::protocol(ProtocolError::Identity))
}

fn identity_encoding(headers: &HeaderMap) -> Result<(), Error> {
    if single(headers, &reqwest::header::CONTENT_ENCODING)?
        .is_some_and(|value| !value.trim().eq_ignore_ascii_case("identity"))
    {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    Ok(())
}

fn validate_headers(headers: &HeaderMap) -> Result<NotificationRequestId, Error> {
    identity_encoding(headers)?;
    let content_type = single(headers, &reqwest::header::CONTENT_TYPE)?
        .ok_or_else(|| Error::protocol(ProtocolError::InvalidFields))?;
    let mut parts = content_type.split(';');
    let mime = parts.next().unwrap().trim();
    let charset = parts.next().and_then(|part| part.trim().split_once('='));
    if !mime.eq_ignore_ascii_case("text/event-stream")
        || parts.next().is_some()
        || !charset.is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("charset")
                && (value.trim().eq_ignore_ascii_case("utf-8")
                    || value.trim().eq_ignore_ascii_case("\"utf-8\""))
        })
    {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    let mut no_store = false;
    let mut no_transform = false;
    for value in headers.get_all(reqwest::header::CACHE_CONTROL) {
        let value = value
            .to_str()
            .map_err(|_| Error::protocol(ProtocolError::InvalidFields))?;
        for directive in value.split(',').map(str::trim) {
            no_store |= directive.eq_ignore_ascii_case("no-store");
            no_transform |= directive.eq_ignore_ascii_case("no-transform");
        }
    }
    if !no_store || !no_transform {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    request_id(headers)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    error: ErrorDetail,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorDetail {
    code: String,
    message: String,
}

async fn response_error(mut response: Response, options: &HttpNotificationOptions) -> Error {
    if matches!(response.status().as_u16(), 404 | 405 | 501) {
        return Error::local(NotificationFailureKind::Unsupported);
    }
    match read_error(&mut response, options).await {
        Ok(error) | Err(error) => error,
    }
}

async fn read_error(
    response: &mut Response,
    options: &HttpNotificationOptions,
) -> Result<Error, Error> {
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    identity_encoding(response.headers())?;
    let request_id = request_id(response.headers())?;
    let content_type = single(response.headers(), &reqwest::header::CONTENT_TYPE)?
        .ok_or_else(|| Error::protocol(ProtocolError::InvalidFields))?;
    if !content_type
        .split(';')
        .next()
        .unwrap()
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    let retry_after = retry_after(response.headers(), options)?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_NOTIFICATION_WIRE_BYTES as u64)
    {
        return Err(Error::protocol(ProtocolError::ByteLimit));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(MAX_NOTIFICATION_WIRE_BYTES)
        .map_err(|_| Error::local(NotificationFailureKind::Capacity))?;
    while let Some(chunk) = response.chunk().await.map_err(Error::transport)? {
        if chunk.len() > options.max_transport_chunk_bytes {
            return Err(Error::local(NotificationFailureKind::Capacity));
        }
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|len| len > MAX_NOTIFICATION_WIRE_BYTES)
        {
            return Err(Error::protocol(ProtocolError::ByteLimit));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = json::validate(&bytes).map_err(Error::protocol)?;
    let envelope: ErrorEnvelope =
        serde_json::from_str(text).map_err(|_| Error::protocol(ProtocolError::InvalidFields))?;
    if envelope.request_id != request_id.as_str() {
        return Err(Error::protocol(ProtocolError::Identity));
    }
    if envelope.error.code.is_empty()
        || envelope.error.code.len() > 64
        || !envelope
            .error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    Ok(Error::http(
        status,
        envelope.error.code,
        envelope.error.message,
        request_id,
        retry_after,
    ))
}

fn retry_after(
    headers: &HeaderMap,
    options: &HttpNotificationOptions,
) -> Result<Option<Duration>, Error> {
    let Some(raw) = single(headers, &reqwest::header::RETRY_AFTER)? else {
        return Ok(None);
    };
    if raw.is_empty() || raw.len() > 128 {
        return Err(Error::protocol(ProtocolError::InvalidFields));
    }
    let delay = if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        Duration::from_secs(
            raw.parse()
                .map_err(|_| Error::protocol(ProtocolError::InvalidFields))?,
        )
    } else {
        httpdate::parse_http_date(raw)
            .map_err(|_| Error::protocol(ProtocolError::InvalidFields))?
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    };
    if delay > Duration::from_millis(options::MAX_TIMER_MS)
        || options
            .retry
            .as_ref()
            .is_some_and(|retry| delay > retry.max_retry_after)
    {
        return Err(Error::protocol(ProtocolError::TimerRange));
    }
    Ok(Some(delay))
}
