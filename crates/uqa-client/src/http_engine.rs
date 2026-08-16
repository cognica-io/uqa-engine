//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::Serialize;
use uqa_sql::{AsyncSQLEngine, SQLParam, SQLResult};

use crate::server_error_envelope::ServerErrorEnvelope;
use crate::sql_batch_execution::SQLBatchWireResponse;
use crate::sql_execution::SQLWireResponse;
use crate::{HttpEngineError, SQLBatchExecution, SQLExecution, SQLStatement, SQLStream};

const JSON_CONTENT_TYPE: &str = "application/json";
const NDJSON_CONTENT_TYPE: &str = "application/x-ndjson";
const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_JSON_RESPONSE_BYTES: usize = 65 * 1024 * 1024;
const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;

/// Authenticated client for the SQL API shared by local and Cloud UQA nodes.
#[derive(Clone)]
pub struct HttpEngine {
    http: Client,
    base_url: Url,
    credential: SecretString,
}

#[derive(Serialize)]
struct SQLBatchRequest<'a> {
    statements: &'a [SQLStatement],
}

impl HttpEngine {
    /// Connect to one UQA data-plane origin.
    ///
    /// Plain HTTP is accepted only for loopback local nodes. Cloud endpoints must use HTTPS.
    pub fn new(base_url: &str, credential: SecretString) -> Result<Self, HttpEngineError> {
        if credential.expose_secret().is_empty() {
            return Err(HttpEngineError::InvalidCredential);
        }
        let base_url = parse_base_url(base_url)?;
        let http = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("uqa-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(HttpEngineError::build_client)?;
        Ok(Self {
            http,
            base_url,
            credential,
        })
    }

    /// Read `UQA_URL` and `UQA_TOKEN`, as emitted by `uqa ... connection --format env`.
    pub fn from_env() -> Result<Self, HttpEngineError> {
        let base_url = std::env::var("UQA_URL")
            .map_err(|_| HttpEngineError::MissingEnvironmentVariable("UQA_URL"))?;
        let credential = std::env::var("UQA_TOKEN")
            .map_err(|_| HttpEngineError::MissingEnvironmentVariable("UQA_TOKEN"))?;
        Self::new(&base_url, SecretString::from(credential))
    }

    /// Execute one materialized SQL statement through `POST /v1/sql`.
    pub async fn sql(
        &self,
        query: &str,
        params: &[SQLParam],
    ) -> Result<SQLResult, HttpEngineError> {
        Ok(self.sql_with_metadata(query, params).await?.into_result())
    }

    /// Execute one materialized SQL statement and preserve its request ID.
    pub async fn sql_with_metadata(
        &self,
        query: &str,
        params: &[SQLParam],
    ) -> Result<SQLExecution, HttpEngineError> {
        let statement = SQLStatement::new(query, params)?;
        let response = self
            .authorized(self.http.post(self.endpoint("v1/sql")?))
            .json(&statement)
            .send()
            .await
            .map_err(HttpEngineError::transport)?;
        let request_id = response_request_id(&response)?;
        let result = decode_json_response::<SQLWireResponse>(response).await?;
        validate_request_id(&request_id, &result.request_id)?;
        Ok(SQLExecution::from_wire(result))
    }

    /// Execute every statement atomically through `POST /v1/sql/batch`.
    pub async fn sql_batch(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<Vec<SQLResult>, HttpEngineError> {
        Ok(self
            .sql_batch_with_metadata(statements)
            .await?
            .into_results())
    }

    /// Execute an atomic SQL batch and preserve its request ID.
    pub async fn sql_batch_with_metadata(
        &self,
        statements: &[(&str, &[SQLParam])],
    ) -> Result<SQLBatchExecution, HttpEngineError> {
        let statements = statements
            .iter()
            .map(|(query, params)| SQLStatement::new(*query, params))
            .collect::<Result<Vec<_>, _>>()?;
        let response = self
            .authorized(self.http.post(self.endpoint("v1/sql/batch")?))
            .json(&SQLBatchRequest {
                statements: &statements,
            })
            .send()
            .await
            .map_err(HttpEngineError::transport)?;
        let request_id = response_request_id(&response)?;
        let result = decode_json_response::<SQLBatchWireResponse>(response).await?;
        validate_request_id(&request_id, &result.request_id)?;
        Ok(SQLBatchExecution::from_wire(result))
    }

    /// Start an incremental SQL request through `POST /v1/sql/stream`.
    pub async fn sql_stream(
        &self,
        query: &str,
        params: &[SQLParam],
    ) -> Result<SQLStream, HttpEngineError> {
        let statement = SQLStatement::new(query, params)?;
        let response = self
            .authorized(self.http.post(self.endpoint("v1/sql/stream")?))
            .header(reqwest::header::ACCEPT, NDJSON_CONTENT_TYPE)
            .json(&statement)
            .send()
            .await
            .map_err(HttpEngineError::transport)?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        validate_content_type(&response, NDJSON_CONTENT_TYPE)?;
        let request_id = response_request_id(&response)?;
        Ok(SQLStream::new(response, request_id))
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(self.credential.expose_secret())
    }

    fn endpoint(&self, path: &str) -> Result<Url, HttpEngineError> {
        self.base_url
            .join(path)
            .map_err(|_| HttpEngineError::InvalidBaseURL)
    }
}

impl AsyncSQLEngine for HttpEngine {
    type Error = HttpEngineError;

    async fn sql<'a>(
        &'a self,
        query: &'a str,
        params: &'a [SQLParam],
    ) -> Result<SQLResult, Self::Error> {
        HttpEngine::sql(self, query, params).await
    }

    async fn sql_batch<'a>(
        &'a self,
        statements: &'a [(&'a str, &'a [SQLParam])],
    ) -> Result<Vec<SQLResult>, Self::Error> {
        HttpEngine::sql_batch(self, statements).await
    }
}

impl fmt::Debug for HttpEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpEngine")
            .field("base_url", &"[REDACTED]")
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

fn parse_base_url(source: &str) -> Result<Url, HttpEngineError> {
    let url = Url::parse(source).map_err(|_| HttpEngineError::InvalidBaseURL)?;
    let valid_origin = url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/"
        && url.host_str().is_some();
    if !valid_origin || !matches!(url.scheme(), "http" | "https") {
        return Err(HttpEngineError::InvalidBaseURL);
    }
    if url.scheme() == "http" && !url.host_str().is_some_and(is_loopback_host) {
        return Err(HttpEngineError::InsecureRemoteURL);
    }
    Ok(url)
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

async fn decode_json_response<T: DeserializeOwned>(
    response: Response,
) -> Result<T, HttpEngineError> {
    if !response.status().is_success() {
        return Err(error_from_response(response).await);
    }
    validate_content_type(&response, JSON_CONTENT_TYPE)?;
    let body = read_bounded(response, MAX_JSON_RESPONSE_BYTES).await?;
    serde_json::from_slice(&body).map_err(HttpEngineError::InvalidResponse)
}

async fn error_from_response(response: Response) -> HttpEngineError {
    let status = response.status();
    if let Err(error) = validate_content_type(&response, JSON_CONTENT_TYPE) {
        return error;
    }
    let header_request_id = match response_request_id(&response) {
        Ok(request_id) => request_id,
        Err(error) => return error,
    };
    let body = match read_bounded(response, MAX_ERROR_RESPONSE_BYTES).await {
        Ok(body) => body,
        Err(error) => return error,
    };
    let Ok(envelope) = serde_json::from_slice::<ServerErrorEnvelope>(&body) else {
        return HttpEngineError::Server {
            status,
            code: "HTTP_ERROR".to_owned(),
            message: "UQA returned a non-success response".to_owned(),
            request_id: Some(header_request_id),
        };
    };
    if header_request_id != envelope.request_id {
        return HttpEngineError::ResponseRequestIdMismatch;
    }
    HttpEngineError::Server {
        status,
        code: envelope.error.code,
        message: envelope.error.message,
        request_id: Some(envelope.request_id),
    }
}

async fn read_bounded(
    mut response: Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, HttpEngineError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        return Err(HttpEngineError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(HttpEngineError::transport)? {
        if body.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(HttpEngineError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_content_type(response: &Response, expected: &str) -> Result<(), HttpEngineError> {
    let valid = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected));
    if valid {
        Ok(())
    } else {
        Err(HttpEngineError::UnexpectedContentType)
    }
}

fn response_request_id(response: &Response) -> Result<String, HttpEngineError> {
    response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(HttpEngineError::MissingRequestId)
}

fn validate_request_id(header: &str, body: &str) -> Result<(), HttpEngineError> {
    if header == body {
        Ok(())
    } else {
        Err(HttpEngineError::ResponseRequestIdMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rejects_credentials_paths_and_remote_plain_http() {
        for source in [
            "http://user:secret@127.0.0.1:8432/",
            "http://127.0.0.1:8432/v1",
            "http://example.com/",
            "ftp://127.0.0.1/",
        ] {
            assert!(parse_base_url(source).is_err(), "accepted {source}");
        }
        assert!(parse_base_url("http://127.0.0.1:8432/").is_ok());
        assert!(parse_base_url("http://[::1]:8432/").is_ok());
        assert!(parse_base_url("https://example.com/").is_ok());
    }

    #[test]
    fn debug_output_redacts_endpoint_and_credential() {
        let credential = "uqa_db_customer-secret";
        let client =
            HttpEngine::new("http://127.0.0.1:8432/", SecretString::from(credential)).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("127.0.0.1"));
        assert!(!debug.contains(credential));
    }
}
