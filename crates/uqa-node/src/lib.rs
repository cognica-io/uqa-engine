//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node.js bindings for the UQA engine.
//!
//! Heavy operations (SQL, searches, calibration) are exposed as
//! Promise-returning methods executed on the libuv thread pool so the
//! JavaScript event loop never blocks; `*Sync` variants exist for
//! scripts and tests. User-defined SQL functions are not yet exposed:
//! calling back into JavaScript from engine execution threads needs a
//! threadsafe-function design that is planned separately.

// The Node-API value-conversion traits (`ToNapiValue` / `FromNapiValue`)
// are `unsafe fn`s over raw `napi_env` pointers; implementing them is
// the whole point of this crate, so the workspace-wide `unsafe_code`
// deny is relaxed here.
#![allow(unsafe_code)]
// napi-exported functions must take owned `String` arguments.
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::JsValue as NapiJsValue;
use napi_derive::napi;

use uqa_core::Value;
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{
    Engine as CoreEngine, HybridSearchParams, SQLParam as CoreSQLParam, SQLResult as CoreSQLResult,
    ScoredEntry, ScoringMode,
};
use uqa_scoring::{BM25Params, CalibrationReport as CoreCalibrationReport};
use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

// ---------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------

/// Bidirectional bridge between engine [`Value`]s and JavaScript
/// values. Ints beyond `Number.MAX_SAFE_INTEGER` surface as `BigInt`,
/// bytes as `Buffer`, decimals and temporals as their SQL strings.
pub struct JSValue(Value);

impl TypeName for JSValue {
    fn type_name() -> &'static str {
        "unknown"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ValidateNapiValue for JSValue {}

impl ToNapiValue for JSValue {
    unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> Result<sys::napi_value> {
        unsafe { value_to_napi(env, val.0) }
    }
}

impl FromNapiValue for JSValue {
    unsafe fn from_napi_value(env: sys::napi_env, napi_val: sys::napi_value) -> Result<Self> {
        let unknown = unsafe { Unknown::from_raw_unchecked(env, napi_val) };
        Ok(Self(value_from_unknown(&unknown)?))
    }
}

unsafe fn value_to_napi(env: sys::napi_env, value: Value) -> Result<sys::napi_value> {
    unsafe {
        match value {
            Value::Null => Null::to_napi_value(env, Null),
            Value::Bool(value) => bool::to_napi_value(env, value),
            Value::Int(value) => {
                if value.abs() <= MAX_SAFE_INTEGER {
                    i64::to_napi_value(env, value)
                } else {
                    BigInt::to_napi_value(env, BigInt::from(value))
                }
            }
            Value::Float(value) => f64::to_napi_value(env, value),
            Value::Decimal(value) => String::to_napi_value(env, value.to_sql_string()),
            Value::Str(value) => String::to_napi_value(env, value),
            Value::Bytes(value) => Buffer::to_napi_value(env, Buffer::from(value)),
            Value::Temporal(value) => String::to_napi_value(env, value.to_sql_string()),
            Value::List(values) => {
                Vec::<JSValue>::to_napi_value(env, values.into_iter().map(JSValue).collect())
            }
            Value::Map(values) => BTreeMap::<String, JSValue>::to_napi_value(
                env,
                values
                    .into_iter()
                    .map(|(key, value)| (key, JSValue(value)))
                    .collect(),
            ),
        }
    }
}

fn value_from_unknown(value: &Unknown<'_>) -> Result<Value> {
    match value.get_type()? {
        ValueType::Undefined | ValueType::Null => Ok(Value::Null),
        ValueType::Boolean => Ok(Value::Bool(unsafe { value.cast::<bool>() }?)),
        ValueType::Number => {
            let number = unsafe { value.cast::<f64>() }?;
            if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER as f64 {
                Ok(Value::Int(number as i64))
            } else {
                Ok(Value::Float(number))
            }
        }
        ValueType::BigInt => {
            let bigint = unsafe { value.cast::<BigInt>() }?;
            let (number, lossless) = bigint.get_i64();
            if lossless {
                Ok(Value::Int(number))
            } else {
                Err(Error::from_reason("BigInt value is outside i64 range"))
            }
        }
        ValueType::String => Ok(Value::Str(unsafe { value.cast::<String>() }?)),
        ValueType::Object => {
            if value.is_buffer()? {
                let buffer = unsafe { value.cast::<Buffer>() }?;
                return Ok(Value::Bytes(buffer.to_vec()));
            }
            if value.is_typedarray()? {
                if let Ok(bytes) = unsafe { value.cast::<Uint8Array>() } {
                    return Ok(Value::Bytes(bytes.to_vec()));
                }
                if let Ok(floats) = unsafe { value.cast::<Float64Array>() } {
                    return Ok(Value::List(
                        floats.iter().map(|value| Value::Float(*value)).collect(),
                    ));
                }
                if let Ok(floats) = unsafe { value.cast::<Float32Array>() } {
                    return Ok(Value::List(
                        floats
                            .iter()
                            .map(|value| Value::Float(f64::from(*value)))
                            .collect(),
                    ));
                }
                return Err(Error::from_reason(
                    "unsupported typed array; use Uint8Array, Float32Array, or Float64Array",
                ));
            }
            if value.is_array()? {
                let values = unsafe { value.cast::<Vec<JSValue>>() }?;
                return Ok(Value::List(
                    values.into_iter().map(|value| value.0).collect(),
                ));
            }
            if value.is_date()? {
                return Err(Error::from_reason(
                    "Date values are not supported; pass an ISO 8601 string instead",
                ));
            }
            let object = unsafe { value.cast::<Object>() }?;
            let mut map = BTreeMap::new();
            for key in Object::keys(&object)? {
                let entry: Option<JSValue> = object.get(&key)?;
                map.insert(key, entry.map_or(Value::Null, |value| value.0));
            }
            Ok(Value::Map(map))
        }
        other => Err(Error::from_reason(format!(
            "unsupported JavaScript value type: {other}"
        ))),
    }
}

fn document_from_js(document: BTreeMap<String, JSValue>) -> BTreeMap<String, Value> {
    document
        .into_iter()
        .map(|(key, value)| (key, value.0))
        .collect()
}

// ---------------------------------------------------------------------
// Result objects
// ---------------------------------------------------------------------

#[napi(object, js_name = "SQLResult")]
pub struct SQLResult {
    pub columns: Vec<String>,
    pub rows: Vec<BTreeMap<String, JSValue>>,
    pub affected_rows: i64,
}

impl From<CoreSQLResult> for SQLResult {
    fn from(result: CoreSQLResult) -> Self {
        Self {
            columns: result.columns,
            rows: result
                .rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|(key, value)| (key, JSValue(value)))
                        .collect()
                })
                .collect(),
            affected_rows: result.affected_rows as i64,
        }
    }
}

fn cypher_result(columns: Vec<String>, rows: Vec<BTreeMap<String, Value>>) -> SQLResult {
    SQLResult {
        columns,
        rows: rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(key, value)| (key, JSValue(value)))
                    .collect()
            })
            .collect(),
        affected_rows: 0,
    }
}

#[napi(object)]
pub struct SearchHit {
    pub doc_id: i64,
    pub score: f64,
}

fn search_hits(entries: Vec<ScoredEntry>) -> Vec<SearchHit> {
    entries
        .into_iter()
        .map(|entry| SearchHit {
            doc_id: entry.doc_id as i64,
            score: entry.score,
        })
        .collect()
}

#[napi(object, js_name = "SQLNotice")]
pub struct SQLNotice {
    pub level: String,
    pub message: String,
}

#[napi(object)]
pub struct ReliabilityBin {
    pub avg_predicted: f64,
    pub avg_actual: f64,
    pub count: i64,
}

#[napi(object)]
pub struct CalibrationReport {
    pub ece: f64,
    pub brier: f64,
    pub log_loss: f64,
    pub bins: Vec<ReliabilityBin>,
}

impl From<CoreCalibrationReport> for CalibrationReport {
    fn from(report: CoreCalibrationReport) -> Self {
        Self {
            ece: report.ece,
            brier: report.brier,
            log_loss: report.log_loss,
            bins: report
                .bins
                .into_iter()
                .map(|bin| ReliabilityBin {
                    avg_predicted: bin.avg_predicted,
                    avg_actual: bin.avg_actual,
                    count: bin.count as i64,
                })
                .collect(),
        }
    }
}

#[napi(object)]
pub struct MigrationReport {
    pub source_path: String,
    pub destination_path: String,
    pub tables: i64,
    pub documents: i64,
    pub fts_fields: i64,
    pub vector_fields: i64,
    pub indexes: i64,
    pub analyzers: i64,
    pub table_field_analyzers: i64,
    pub foreign_servers: i64,
    pub foreign_tables: i64,
    pub graphs: i64,
    pub graph_vertices: i64,
    pub graph_edges: i64,
    pub path_indexes: i64,
    pub scoring_params: i64,
    pub models: i64,
    pub column_stats: i64,
}

impl From<PythonMigrationReport> for MigrationReport {
    fn from(report: PythonMigrationReport) -> Self {
        Self {
            source_path: report.source_path.to_string_lossy().into_owned(),
            destination_path: report.destination_path.to_string_lossy().into_owned(),
            tables: report.tables as i64,
            documents: report.documents as i64,
            fts_fields: report.fts_fields as i64,
            vector_fields: report.vector_fields as i64,
            indexes: report.indexes as i64,
            analyzers: report.analyzers as i64,
            table_field_analyzers: report.table_field_analyzers as i64,
            foreign_servers: report.foreign_servers as i64,
            foreign_tables: report.foreign_tables as i64,
            graphs: report.graphs as i64,
            graph_vertices: report.graph_vertices as i64,
            graph_edges: report.graph_edges as i64,
            path_indexes: report.path_indexes as i64,
            scoring_params: report.scoring_params as i64,
            models: report.models as i64,
            column_stats: report.column_stats as i64,
        }
    }
}

#[napi(object)]
pub struct CompressionOptions {
    pub codec: Option<String>,
    pub page_size: Option<u32>,
    pub chunk_pages: Option<u32>,
    pub level: Option<i32>,
}

// ---------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------

#[napi(js_name = "SQLParam")]
pub struct SQLParam {
    inner: CoreSQLParam,
}

#[napi]
impl SQLParam {
    #[napi(factory)]
    pub fn scalar(value: Unknown<'_>) -> Result<SQLParam> {
        Ok(Self {
            inner: CoreSQLParam::scalar(value_from_unknown(&value)?),
        })
    }

    #[napi(factory)]
    pub fn vector(values: Either<Float32Array, Vec<f64>>) -> SQLParam {
        Self {
            inner: CoreSQLParam::vector(vector_from_input(values)),
        }
    }

    #[napi(factory)]
    pub fn tensor(values: Vec<Vec<f64>>) -> SQLParam {
        Self {
            inner: CoreSQLParam::tensor(
                values
                    .into_iter()
                    .map(|row| row.into_iter().map(|value| value as f32).collect())
                    .collect(),
            ),
        }
    }
}

fn vector_from_input(values: Either<Float32Array, Vec<f64>>) -> Vec<f32> {
    match values {
        Either::A(values) => values.to_vec(),
        Either::B(values) => values.into_iter().map(|value| value as f32).collect(),
    }
}

type ParamInput<'env> = Either<ClassInstance<'env, SQLParam>, Unknown<'env>>;

fn params_from_input(params: Option<Vec<ParamInput<'_>>>) -> Result<Vec<CoreSQLParam>> {
    params
        .unwrap_or_default()
        .into_iter()
        .map(|param| match param {
            Either::A(instance) => Ok(instance.inner.clone()),
            Either::B(unknown) => Ok(CoreSQLParam::scalar(value_from_unknown(&unknown)?)),
        })
        .collect()
}

fn labels_from_input(labels: Vec<u32>) -> Result<Vec<u8>> {
    labels
        .into_iter()
        .map(|label| {
            if label > 1 {
                Err(Error::from_reason("labels must contain only 0 or 1"))
            } else {
                Ok(label as u8)
            }
        })
        .collect()
}

fn doc_id_from_input(doc_id: i64) -> Result<u64> {
    u64::try_from(doc_id).map_err(|_| Error::from_reason("docId must be non-negative"))
}

// ---------------------------------------------------------------------
// Async tasks
// ---------------------------------------------------------------------

pub struct SQLTask {
    engine: Arc<CoreEngine>,
    query: String,
    params: Vec<CoreSQLParam>,
}

impl Task for SQLTask {
    type Output = CoreSQLResult;
    type JsValue = SQLResult;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .sql(&self.query, &self.params)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct SQLBatchTask {
    engine: Arc<CoreEngine>,
    statements: Vec<(String, Vec<CoreSQLParam>)>,
}

impl Task for SQLBatchTask {
    type Output = Vec<CoreSQLResult>;
    type JsValue = Vec<SQLResult>;

    fn compute(&mut self) -> Result<Self::Output> {
        let borrowed: Vec<(&str, &[CoreSQLParam])> = self
            .statements
            .iter()
            .map(|(sql, params)| (sql.as_str(), params.as_slice()))
            .collect();
        self.engine.sql_batch(&borrowed).map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into_iter().map(Into::into).collect())
    }
}

pub struct SearchTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    query: String,
    top_k: usize,
    scoring: String,
}

impl Task for SearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        let mode = scoring_mode(&self.engine, &self.table, &self.field, &self.scoring)?;
        Ok(self
            .engine
            .search(&self.table, &self.field, &self.query, &mode, self.top_k))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(search_hits(output))
    }
}

pub struct KNNSearchTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    vector: Vec<f32>,
    top_k: usize,
}

impl Task for KNNSearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self
            .engine
            .knn_search(&self.table, &self.field, &self.vector, self.top_k))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(search_hits(output))
    }
}

pub struct VectorSimilarityTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    vector: Vec<f32>,
    threshold: f32,
}

impl Task for VectorSimilarityTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.engine.vector_similarity_search(
            &self.table,
            &self.field,
            self.vector.clone(),
            self.threshold,
        ))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(search_hits(output))
    }
}

pub struct HybridSearchTask {
    engine: Arc<CoreEngine>,
    table: String,
    text_field: String,
    text_query: String,
    vector_field: String,
    query_vector: Vec<f32>,
    top_k: usize,
    knn_pool: usize,
    alpha: f64,
}

impl Task for HybridSearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        let params = HybridSearchParams {
            table: &self.table,
            text_field: &self.text_field,
            text_query: &self.text_query,
            vector_field: &self.vector_field,
            query_vector: self.query_vector.clone(),
            knn_pool: self.knn_pool,
            alpha: self.alpha,
            top_k: self.top_k,
        };
        Ok(self.engine.hybrid_search(&params))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(search_hits(output))
    }
}

pub struct RunCypherTask {
    engine: Arc<CoreEngine>,
    graph: String,
    query: String,
    params: BTreeMap<String, Value>,
}

impl Task for RunCypherTask {
    type Output = (Vec<String>, Vec<BTreeMap<String, Value>>);
    type JsValue = SQLResult;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .run_cypher(&self.graph, &self.query, self.params.clone())
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(cypher_result(output.0, output.1))
    }
}

pub struct EstimateScoringParamsTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    n_samples: usize,
    tokens_per_query: usize,
    seed: i64,
}

impl Task for EstimateScoringParamsTask {
    type Output = BTreeMap<String, f64>;
    type JsValue = BTreeMap<String, f64>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .estimate_scoring_params(
                &self.table,
                &self.field,
                self.n_samples,
                self.tokens_per_query,
                self.seed,
            )
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct LearnScoringParamsTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    query: String,
    labels: Vec<u8>,
}

impl Task for LearnScoringParamsTask {
    type Output = BTreeMap<String, f64>;
    type JsValue = BTreeMap<String, f64>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .learn_scoring_params(&self.table, &self.field, &self.query, &self.labels)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CalibrationReportTask {
    engine: Arc<CoreEngine>,
    table: String,
    field: String,
    query: String,
    labels: Vec<u8>,
}

impl Task for CalibrationReportTask {
    type Output = CoreCalibrationReport;
    type JsValue = CalibrationReport;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .calibration_report(&self.table, &self.field, &self.query, &self.labels)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

// ---------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------

#[napi]
pub struct Engine {
    inner: Arc<CoreEngine>,
}

#[napi]
impl Engine {
    /// Create an in-memory engine.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CoreEngine::new()),
        }
    }

    #[napi(factory)]
    pub fn open(path: String) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(CoreEngine::open(Path::new(&path)).map_err(runtime_error)?),
        })
    }

    #[napi(factory)]
    pub fn open_encrypted(path: String, key: String) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(
                CoreEngine::open_encrypted(Path::new(&path), &key).map_err(runtime_error)?,
            ),
        })
    }

    #[napi(factory)]
    pub fn open_auto(path: String, key: Option<String>) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(
                CoreEngine::open_auto(Path::new(&path), key.as_deref()).map_err(runtime_error)?,
            ),
        })
    }

    #[napi(factory)]
    pub fn open_compressed(path: String, options: Option<CompressionOptions>) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(
                CoreEngine::open_compressed(Path::new(&path), compression_options(options)?)
                    .map_err(runtime_error)?,
            ),
        })
    }

    #[napi(factory)]
    pub fn open_compressed_encrypted(
        path: String,
        key: String,
        options: Option<CompressionOptions>,
    ) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(
                CoreEngine::open_compressed_encrypted(
                    Path::new(&path),
                    &key,
                    compression_options(options)?,
                )
                .map_err(runtime_error)?,
            ),
        })
    }

    #[napi]
    pub fn detect_database_file(path: String) -> Result<String> {
        let format = CoreEngine::detect_database_file(Path::new(&path))
            .map_err(|err| Error::from_reason(err.to_string()))?;
        Ok(database_file_format_name(format).to_string())
    }

    #[napi(ts_return_type = "Promise<SQLResult>")]
    pub fn sql(
        &self,
        query: String,
        params: Option<Vec<ParamInput>>,
    ) -> Result<AsyncTask<SQLTask>> {
        Ok(AsyncTask::new(SQLTask {
            engine: self.inner.clone(),
            query,
            params: params_from_input(params)?,
        }))
    }

    #[napi]
    pub fn sql_sync(&self, query: String, params: Option<Vec<ParamInput>>) -> Result<SQLResult> {
        let params = params_from_input(params)?;
        Ok(self
            .inner
            .sql(&query, &params)
            .map_err(runtime_error)?
            .into())
    }

    #[napi(ts_return_type = "Promise<Array<SQLResult>>")]
    pub fn sql_batch(
        &self,
        statements: Vec<(String, Vec<ParamInput>)>,
    ) -> Result<AsyncTask<SQLBatchTask>> {
        Ok(AsyncTask::new(SQLBatchTask {
            engine: self.inner.clone(),
            statements: batch_from_input(statements)?,
        }))
    }

    #[napi]
    pub fn sql_batch_sync(
        &self,
        statements: Vec<(String, Vec<ParamInput>)>,
    ) -> Result<Vec<SQLResult>> {
        let statements = batch_from_input(statements)?;
        let borrowed: Vec<(&str, &[CoreSQLParam])> = statements
            .iter()
            .map(|(sql, params)| (sql.as_str(), params.as_slice()))
            .collect();
        Ok(self
            .inner
            .sql_batch(&borrowed)
            .map_err(runtime_error)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    #[napi]
    pub fn create_default_table(&self, name: String, fts_fields: Vec<String>) {
        self.inner.create_default_table(&name, fts_fields);
    }

    #[napi]
    pub fn create_vector_field(&self, table: String, field: String, dimensions: u32) -> bool {
        self.inner.create_vector_field(&table, &field, dimensions)
    }

    #[napi]
    pub fn add_document(
        &self,
        table: String,
        doc_id: i64,
        document: BTreeMap<String, JSValue>,
    ) -> Result<()> {
        self.inner
            .add_document(
                &table,
                doc_id_from_input(doc_id)?,
                document_from_js(document),
            )
            .map_err(runtime_error)
    }

    #[napi]
    pub fn add_document_with_vectors(
        &self,
        table: String,
        doc_id: i64,
        document: BTreeMap<String, JSValue>,
        vectors: BTreeMap<String, Either<Vec<f64>, Vec<Vec<f64>>>>,
    ) -> Result<()> {
        let vectors = vectors
            .into_iter()
            .map(|(field, values)| {
                let rows = match values {
                    Either::A(single) => {
                        vec![single.into_iter().map(|value| value as f32).collect()]
                    }
                    Either::B(rows) => rows
                        .into_iter()
                        .map(|row| row.into_iter().map(|value| value as f32).collect())
                        .collect(),
                };
                (field, rows)
            })
            .collect();
        self.inner
            .add_document_with_vector_values(
                &table,
                doc_id_from_input(doc_id)?,
                document_from_js(document),
                vectors,
            )
            .map_err(runtime_error)
    }

    #[napi]
    pub fn add_vector(
        &self,
        table: String,
        doc_id: i64,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
    ) -> Result<bool> {
        Ok(self.inner.add_vector(
            &table,
            doc_id_from_input(doc_id)?,
            &field,
            vector_from_input(vector),
        ))
    }

    #[napi]
    pub fn add_vector_values(
        &self,
        table: String,
        doc_id: i64,
        field: String,
        vectors: Vec<Vec<f64>>,
    ) -> Result<bool> {
        Ok(self.inner.add_vector_values(
            &table,
            doc_id_from_input(doc_id)?,
            &field,
            vectors
                .into_iter()
                .map(|row| row.into_iter().map(|value| value as f32).collect())
                .collect(),
        ))
    }

    #[napi]
    pub fn get_document(
        &self,
        table: String,
        doc_id: i64,
    ) -> Result<Option<BTreeMap<String, JSValue>>> {
        Ok(self
            .inner
            .get_document(&table, doc_id_from_input(doc_id)?)
            .map(|document| {
                document
                    .into_iter()
                    .map(|(key, value)| (key, JSValue(value)))
                    .collect()
            }))
    }

    #[napi]
    pub fn delete_document(&self, table: String, doc_id: i64) -> Result<()> {
        self.inner
            .delete_document(&table, doc_id_from_input(doc_id)?)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn document_count(&self, table: String) -> i64 {
        self.inner.document_count(&table) as i64
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn search(
        &self,
        table: String,
        field: String,
        query: String,
        top_k: Option<u32>,
        scoring: Option<String>,
    ) -> AsyncTask<SearchTask> {
        AsyncTask::new(SearchTask {
            engine: self.inner.clone(),
            table,
            field,
            query,
            top_k: top_k.unwrap_or(10) as usize,
            scoring: scoring.unwrap_or_else(|| "bm25".to_string()),
        })
    }

    #[napi]
    pub fn search_sync(
        &self,
        table: String,
        field: String,
        query: String,
        top_k: Option<u32>,
        scoring: Option<String>,
    ) -> Result<Vec<SearchHit>> {
        let scoring = scoring.unwrap_or_else(|| "bm25".to_string());
        let mode = scoring_mode(&self.inner, &table, &field, &scoring)?;
        Ok(search_hits(self.inner.search(
            &table,
            &field,
            &query,
            &mode,
            top_k.unwrap_or(10) as usize,
        )))
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn knn_search(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
    ) -> AsyncTask<KNNSearchTask> {
        AsyncTask::new(KNNSearchTask {
            engine: self.inner.clone(),
            table,
            field,
            vector: vector_from_input(vector),
            top_k: top_k.unwrap_or(10) as usize,
        })
    }

    #[napi]
    pub fn knn_search_sync(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
    ) -> Vec<SearchHit> {
        search_hits(self.inner.knn_search(
            &table,
            &field,
            vector_from_input(vector),
            top_k.unwrap_or(10) as usize,
        ))
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn vector_similarity_search(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        threshold: f64,
    ) -> AsyncTask<VectorSimilarityTask> {
        AsyncTask::new(VectorSimilarityTask {
            engine: self.inner.clone(),
            table,
            field,
            vector: vector_from_input(vector),
            threshold: threshold as f32,
        })
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search(
        &self,
        table: String,
        text_field: String,
        text_query: String,
        vector_field: String,
        query_vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
        knn_pool: Option<u32>,
        alpha: Option<f64>,
    ) -> AsyncTask<HybridSearchTask> {
        let top_k = top_k.unwrap_or(10) as usize;
        AsyncTask::new(HybridSearchTask {
            engine: self.inner.clone(),
            table,
            text_field,
            text_query,
            vector_field,
            query_vector: vector_from_input(query_vector),
            top_k,
            knn_pool: knn_pool
                .map_or_else(|| top_k.saturating_mul(4).max(top_k), |pool| pool as usize),
            alpha: alpha.unwrap_or(1.0),
        })
    }

    #[napi(ts_return_type = "Promise<Record<string, number>>")]
    pub fn estimate_scoring_params(
        &self,
        table: String,
        field: String,
        n_samples: Option<u32>,
        tokens_per_query: Option<u32>,
        seed: Option<i64>,
    ) -> AsyncTask<EstimateScoringParamsTask> {
        AsyncTask::new(EstimateScoringParamsTask {
            engine: self.inner.clone(),
            table,
            field,
            n_samples: n_samples.unwrap_or(50) as usize,
            tokens_per_query: tokens_per_query.unwrap_or(5) as usize,
            seed: seed.unwrap_or(42),
        })
    }

    #[napi(ts_return_type = "Promise<Record<string, number>>")]
    pub fn learn_scoring_params(
        &self,
        table: String,
        field: String,
        query: String,
        labels: Vec<u32>,
    ) -> Result<AsyncTask<LearnScoringParamsTask>> {
        Ok(AsyncTask::new(LearnScoringParamsTask {
            engine: self.inner.clone(),
            table,
            field,
            query,
            labels: labels_from_input(labels)?,
        }))
    }

    #[napi]
    pub fn update_scoring_params(
        &self,
        table: String,
        field: String,
        score: f64,
        label: u32,
    ) -> Result<()> {
        let label = labels_from_input(vec![label])?[0];
        self.inner
            .update_scoring_params(&table, &field, score, label)
            .map_err(runtime_error)
    }

    #[napi(ts_return_type = "Promise<CalibrationReport>")]
    pub fn calibration_report(
        &self,
        table: String,
        field: String,
        query: String,
        labels: Vec<u32>,
    ) -> Result<AsyncTask<CalibrationReportTask>> {
        Ok(AsyncTask::new(CalibrationReportTask {
            engine: self.inner.clone(),
            table,
            field,
            query,
            labels: labels_from_input(labels)?,
        }))
    }

    #[napi]
    pub fn save_scoring_params(&self, name: String, params: BTreeMap<String, f64>) -> Result<()> {
        let json = serde_json::to_string(&params)
            .map_err(|err| Error::from_reason(format!("serialize scoring params: {err}")))?;
        self.inner
            .save_scoring_params(&name, &json)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn load_scoring_params(&self, name: String) -> Result<Option<BTreeMap<String, f64>>> {
        self.inner
            .load_scoring_params(&name)
            .map(|json| parse_scoring_params(&name, &json))
            .transpose()
    }

    #[napi]
    pub fn load_all_scoring_params(&self) -> Result<BTreeMap<String, BTreeMap<String, f64>>> {
        self.inner
            .load_all_scoring_params()
            .into_iter()
            .map(|(name, json)| {
                let params = parse_scoring_params(&name, &json)?;
                Ok((name, params))
            })
            .collect()
    }

    #[napi]
    pub fn drop_scoring_params(&self, name: String) -> bool {
        self.inner.drop_scoring_params(&name)
    }

    #[napi(ts_return_type = "Promise<SQLResult>")]
    pub fn run_cypher(
        &self,
        graph: String,
        query: String,
        params: Option<BTreeMap<String, JSValue>>,
    ) -> AsyncTask<RunCypherTask> {
        AsyncTask::new(RunCypherTask {
            engine: self.inner.clone(),
            graph,
            query,
            params: params.map(document_from_js).unwrap_or_default(),
        })
    }

    #[napi]
    pub fn run_cypher_sync(
        &self,
        graph: String,
        query: String,
        params: Option<BTreeMap<String, JSValue>>,
    ) -> Result<SQLResult> {
        let params = params.map(document_from_js).unwrap_or_default();
        let (columns, rows) = self
            .inner
            .run_cypher(&graph, &query, params)
            .map_err(runtime_error)?;
        Ok(cypher_result(columns, rows))
    }

    #[napi]
    pub fn create_graph(&self, name: String) {
        self.inner.create_graph(&name);
    }

    #[napi]
    pub fn drop_graph(&self, name: String) {
        self.inner.drop_graph(&name);
    }

    #[napi]
    pub fn list_graphs(&self) -> Vec<String> {
        self.inner.list_graphs()
    }

    #[napi]
    pub fn list_path_indexes(&self) -> Vec<String> {
        self.inner.list_path_indexes()
    }

    #[napi]
    pub fn table_names(&self) -> Vec<String> {
        self.inner.table_names()
    }

    #[napi]
    pub fn list_views(&self) -> Vec<String> {
        self.inner.list_views()
    }

    #[napi]
    pub fn list_schemas(&self) -> Vec<String> {
        self.inner.list_schemas()
    }

    #[napi]
    pub fn list_sequences(&self) -> Vec<String> {
        self.inner.list_sequences()
    }

    #[napi]
    pub fn list_named_analyzers(&self) -> Vec<String> {
        self.inner.list_named_analyzers()
    }

    #[napi]
    pub fn list_foreign_servers(&self) -> Vec<String> {
        self.inner.list_foreign_servers()
    }

    #[napi]
    pub fn list_foreign_tables(&self) -> Vec<String> {
        self.inner.list_foreign_tables()
    }

    #[napi(js_name = "takeSQLNotices")]
    pub fn take_sql_notices(&self) -> Vec<SQLNotice> {
        self.inner
            .take_sql_notices()
            .into_iter()
            .map(|(level, message)| SQLNotice { level, message })
            .collect()
    }

    #[napi(js_name = "sqlFunctionDepthLimit")]
    pub fn sql_function_depth_limit(&self) -> u32 {
        self.inner.sql_function_depth_limit() as u32
    }

    #[napi(js_name = "setSQLFunctionDepthLimit")]
    pub fn set_sql_function_depth_limit(&self, limit: u32) {
        self.inner.set_sql_function_depth_limit(limit as usize);
    }

    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    #[napi]
    pub fn close(&self) {
        self.inner.close();
    }
}

// ---------------------------------------------------------------------
// Module functions
// ---------------------------------------------------------------------

#[napi]
pub fn open(path: String) -> Result<Engine> {
    Engine::open(path)
}

#[napi]
pub fn open_encrypted(path: String, key: String) -> Result<Engine> {
    Engine::open_encrypted(path, key)
}

#[napi]
pub fn open_auto(path: String, key: Option<String>) -> Result<Engine> {
    Engine::open_auto(path, key)
}

#[napi]
pub fn open_compressed(path: String, options: Option<CompressionOptions>) -> Result<Engine> {
    Engine::open_compressed(path, options)
}

#[napi]
pub fn open_compressed_encrypted(
    path: String,
    key: String,
    options: Option<CompressionOptions>,
) -> Result<Engine> {
    Engine::open_compressed_encrypted(path, key, options)
}

#[napi]
pub fn detect_database_file(path: String) -> Result<String> {
    Engine::detect_database_file(path)
}

#[napi]
pub fn vector(values: Either<Float32Array, Vec<f64>>) -> SQLParam {
    SQLParam::vector(values)
}

#[napi]
pub fn tensor(values: Vec<Vec<f64>>) -> SQLParam {
    SQLParam::tensor(values)
}

#[napi(js_name = "migratePythonDB")]
pub fn migrate_python_db(source: String, destination: String) -> Result<MigrationReport> {
    migrate_python_database(Path::new(&source), Path::new(&destination))
        .map(Into::into)
        .map_err(runtime_error)
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

fn batch_from_input(
    statements: Vec<(String, Vec<ParamInput<'_>>)>,
) -> Result<Vec<(String, Vec<CoreSQLParam>)>> {
    statements
        .into_iter()
        .map(|(sql, params)| Ok((sql, params_from_input(Some(params))?)))
        .collect()
}

fn compression_options(options: Option<CompressionOptions>) -> Result<SQLiteCompressionOptions> {
    let options = options.unwrap_or(CompressionOptions {
        codec: None,
        page_size: None,
        chunk_pages: None,
        level: None,
    });
    let codec = options.codec.as_deref().unwrap_or("zstd");
    let mut resolved = match codec.to_ascii_lowercase().as_str() {
        "zstd" => SQLiteCompressionOptions::zstd(),
        "lz4" => SQLiteCompressionOptions::lz4(),
        other => {
            return Err(Error::from_reason(format!(
                "unsupported compression codec `{other}`"
            )));
        }
    };
    if let Some(value) = options.page_size {
        resolved.page_size = value;
    }
    if let Some(value) = options.chunk_pages {
        resolved.chunk_pages = value;
    }
    if let Some(value) = options.level {
        resolved.level = value;
    }
    resolved.validate().map_err(Error::from_reason)
}

fn scoring_mode(
    engine: &CoreEngine,
    table: &str,
    field: &str,
    scoring: &str,
) -> Result<ScoringMode> {
    match scoring.to_ascii_lowercase().as_str() {
        "bm25" => Ok(ScoringMode::BM25(BM25Params::default())),
        "bayesian" | "bayesian_bm25" => Ok(ScoringMode::BayesianBM25(
            engine.bayesian_params_for(table, field),
        )),
        other => Err(Error::from_reason(format!(
            "unsupported scoring mode `{other}`"
        ))),
    }
}

fn database_file_format_name(format: DatabaseFileFormat) -> &'static str {
    match format {
        DatabaseFileFormat::Missing => "missing",
        DatabaseFileFormat::PlainSQLite => "sqlite",
        DatabaseFileFormat::CompressedContainer { encrypted: false } => "compressed",
        DatabaseFileFormat::CompressedContainer { encrypted: true } => "compressed_encrypted",
        DatabaseFileFormat::Unrecognized => "unrecognized",
    }
}

fn parse_scoring_params(name: &str, json: &str) -> Result<BTreeMap<String, f64>> {
    serde_json::from_str(json).map_err(|err| {
        Error::from_reason(format!(
            "scoring params `{name}` are not a map of floats: {err}"
        ))
    })
}

fn runtime_error(error: impl std::fmt::Display) -> Error {
    Error::from_reason(error.to_string())
}
