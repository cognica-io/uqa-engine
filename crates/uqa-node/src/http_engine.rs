//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node-API binding for authenticated local and Cloud HTTP SQL.

use std::sync::Arc;

use napi::bindgen_prelude::{
    AsyncBlock, AsyncBlockBuilder, Env, Error, Object, Result, ToNapiValue,
};
use napi::sys;
use napi_derive::napi;
use tokio::sync::Mutex;
use uqa_client::{
    HttpEngine as CoreHttpEngine, HttpEngineError, SQLStream as CoreSQLStream,
    SQLStreamFrame as CoreSQLStreamFrame, SecretString,
};

use crate::input::{batch_from_input, js_number_from_usize, params_from_input, ParamInput};
use crate::results::SQLResult;
use crate::value::JSRow;

#[napi]
pub struct HttpEngine {
    inner: Arc<CoreHttpEngine>,
}

#[napi(js_name = "HttpSQLStream")]
pub struct HttpSQLStream {
    inner: Arc<Mutex<CoreSQLStream>>,
    request_id: String,
}

#[napi(object, js_name = "HttpSQLExecution")]
pub struct HttpSQLExecution {
    pub result: SQLResult,
    pub request_id: String,
}

#[napi(object, js_name = "HttpSQLBatchExecution")]
pub struct HttpSQLBatchExecution {
    pub results: Vec<SQLResult>,
    pub request_id: String,
}

#[napi(object, object_from_js = false, js_name = "HttpSQLStreamFrame")]
pub struct HttpSQLStreamFrame {
    pub r#type: String,
    pub columns: Option<Vec<String>>,
    #[napi(ts_type = "Record<string, JSValue>")]
    pub row: Option<JSRow>,
    pub row_count: Option<i64>,
    pub spilled_to_disk: Option<bool>,
    pub request_id: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    #[napi(ts_type = "import(\"./http\").HttpSQLDiagnostic")]
    pub diagnostic: Option<HttpSQLDiagnostic>,
}

pub struct HttpSQLDiagnostic(uqa_client::SQLDiagnostic);

impl ToNapiValue for HttpSQLDiagnostic {
    unsafe fn to_napi_value(raw: sys::napi_env, value: Self) -> Result<sys::napi_value> {
        let env = Env::from_raw(raw);
        let mut object = Object::new(&env)?;
        let diagnostic = value.0;
        if let Some(sqlstate) = diagnostic.sqlstate() {
            object.set("sqlstate", sqlstate)?;
        }
        object.set("category", diagnostic.category.as_str())?;
        if let Some(index) = diagnostic.statement_index {
            object.set("statementIndex", index)?;
        }
        if let Some(position) = diagnostic.position {
            object.set("position", position)?;
        }
        unsafe { Object::to_napi_value(raw, object) }
    }
}

#[napi(object, js_name = "HttpEngineLocalOptions")]
pub struct HttpEngineLocalOptions {
    pub cli_path: Option<String>,
}

#[napi(object, js_name = "HttpEngineCloudOptions")]
pub struct HttpEngineCloudOptions {
    pub organization: Option<String>,
    pub cli_path: Option<String>,
}

#[napi]
impl HttpEngine {
    /// Connect to one local or Cloud UQA data-plane origin.
    #[napi(constructor)]
    pub fn new(url: String, token: String) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(
                CoreHttpEngine::new(&url, SecretString::from(token)).map_err(http_error)?,
            ),
        })
    }

    /// Read `UQA_URL` and `UQA_TOKEN` from the process environment.
    #[napi(factory)]
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            inner: Arc::new(CoreHttpEngine::from_env().map_err(http_error)?),
        })
    }

    /// Resolve a local project through the installed `uqa` CLI.
    #[napi(factory, ts_return_type = "Promise<HttpEngine>")]
    pub async fn local(project: String, options: Option<HttpEngineLocalOptions>) -> Result<Self> {
        let cli_path = options.and_then(|value| value.cli_path);
        let inner = match cli_path {
            Some(path) => CoreHttpEngine::local_with_cli(&project, path).await,
            None => CoreHttpEngine::local(&project).await,
        }
        .map_err(http_error)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Resolve a Cloud project and optional organization through the installed `uqa` CLI.
    #[napi(factory, ts_return_type = "Promise<HttpEngine>")]
    pub async fn cloud(project: String, options: Option<HttpEngineCloudOptions>) -> Result<Self> {
        let organization = options
            .as_ref()
            .and_then(|value| value.organization.clone());
        let cli_path = options.and_then(|value| value.cli_path);
        let inner = match cli_path {
            Some(path) => {
                CoreHttpEngine::cloud_with_cli(&project, organization.as_deref(), path).await
            }
            None => CoreHttpEngine::cloud(&project, organization.as_deref()).await,
        }
        .map_err(http_error)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    #[napi(ts_return_type = "Promise<SQLResult>")]
    pub fn sql(
        &self,
        env: Env,
        query: String,
        params: Option<Vec<ParamInput<'_>>>,
    ) -> Result<AsyncBlock<SQLResult>> {
        let params = params_from_input(params)?;
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            inner
                .sql(&query, &params)
                .await
                .map_err(http_error)?
                .try_into()
        })
        .build(&env)
    }

    #[napi(ts_return_type = "Promise<HttpSQLExecution>")]
    pub fn sql_with_metadata(
        &self,
        env: Env,
        query: String,
        params: Option<Vec<ParamInput<'_>>>,
    ) -> Result<AsyncBlock<HttpSQLExecution>> {
        let params = params_from_input(params)?;
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            let output = inner
                .sql_with_metadata(&query, &params)
                .await
                .map_err(http_error)?;
            let request_id = output.request_id().to_owned();
            Ok(HttpSQLExecution {
                result: output.into_result().try_into()?,
                request_id,
            })
        })
        .build(&env)
    }

    #[napi(ts_return_type = "Promise<Array<SQLResult>>")]
    pub fn sql_batch(
        &self,
        env: Env,
        statements: Vec<(String, Vec<ParamInput<'_>>)>,
    ) -> Result<AsyncBlock<Vec<SQLResult>>> {
        let statements = batch_from_input(statements)?;
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            let borrowed = uqa_sql::params::borrow_sql_batch(&statements);
            inner
                .sql_batch(&borrowed)
                .await
                .map_err(http_error)?
                .into_iter()
                .map(TryInto::try_into)
                .collect()
        })
        .build(&env)
    }

    #[napi(ts_return_type = "Promise<HttpSQLBatchExecution>")]
    pub fn sql_batch_with_metadata(
        &self,
        env: Env,
        statements: Vec<(String, Vec<ParamInput<'_>>)>,
    ) -> Result<AsyncBlock<HttpSQLBatchExecution>> {
        let statements = batch_from_input(statements)?;
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            let borrowed = uqa_sql::params::borrow_sql_batch(&statements);
            let output = inner
                .sql_batch_with_metadata(&borrowed)
                .await
                .map_err(http_error)?;
            let request_id = output.request_id().to_owned();
            let results = output
                .into_results()
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>>>()?;
            Ok(HttpSQLBatchExecution {
                results,
                request_id,
            })
        })
        .build(&env)
    }

    #[napi(ts_return_type = "Promise<HttpSQLStream>")]
    pub fn sql_stream(
        &self,
        env: Env,
        query: String,
        params: Option<Vec<ParamInput<'_>>>,
    ) -> Result<AsyncBlock<HttpSQLStream>> {
        let params = params_from_input(params)?;
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            let output = inner
                .sql_stream(&query, &params)
                .await
                .map_err(http_error)?;
            let request_id = output.request_id().to_owned();
            Ok(HttpSQLStream {
                inner: Arc::new(Mutex::new(output)),
                request_id,
            })
        })
        .build(&env)
    }
}

#[napi]
impl HttpSQLStream {
    #[napi(getter)]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[napi(ts_return_type = "Promise<HttpSQLStreamFrame | null>")]
    pub async fn next_frame(&self) -> Result<Option<HttpSQLStreamFrame>> {
        self.inner
            .lock()
            .await
            .next_frame()
            .await
            .map_err(http_error)?
            .map(HttpSQLStreamFrame::try_from)
            .transpose()
    }
}

impl TryFrom<CoreSQLStreamFrame> for HttpSQLStreamFrame {
    type Error = Error;

    fn try_from(frame: CoreSQLStreamFrame) -> Result<Self> {
        let mut output = Self {
            r#type: String::new(),
            columns: None,
            row: None,
            row_count: None,
            spilled_to_disk: None,
            request_id: None,
            code: None,
            message: None,
            diagnostic: None,
        };
        match frame {
            CoreSQLStreamFrame::Metadata {
                columns,
                row_count,
                spilled_to_disk,
                request_id,
            } => {
                output.r#type = String::from("metadata");
                output.columns = Some(columns);
                output.row_count = Some(js_number_from_usize(row_count, "stream row count")?);
                output.spilled_to_disk = Some(spilled_to_disk);
                output.request_id = Some(request_id);
            }
            CoreSQLStreamFrame::Row { row } => {
                output.r#type = String::from("row");
                output.row = Some(JSRow(row));
            }
            CoreSQLStreamFrame::Complete {
                row_count,
                request_id,
            } => {
                output.r#type = String::from("complete");
                output.row_count = Some(js_number_from_usize(row_count, "stream row count")?);
                output.request_id = Some(request_id);
            }
            CoreSQLStreamFrame::Error {
                code,
                message,
                request_id,
                diagnostic,
            } => {
                output.r#type = String::from("error");
                output.code = Some(code);
                output.message = Some(message);
                output.request_id = Some(request_id);
                output.diagnostic = diagnostic.map(HttpSQLDiagnostic);
            }
        }
        Ok(output)
    }
}

fn http_error(error: HttpEngineError) -> Error {
    Error::from_reason(error.to_string())
}
