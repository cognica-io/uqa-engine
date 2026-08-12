//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node-API Engine class and synchronous/asynchronous methods.

use super::callbacks::{
    function_options, CallbackArguments, JSAggregateCallbacks, JSAggregateFunction,
    JSFunctionOptions, JSScalarFunction, JSTableFunction, SynchronousJSValue,
};
use super::input::{
    batch_from_input, compression_options, database_file_format_name, doc_id_from_input,
    f32_from_f64, js_number_from_u64, labels_from_input, params_from_input, parse_scoring_params,
    runtime_error, scoring_mode, tensor_from_f64, usize_from_u32, vector_from_f64,
    vector_from_input, ParamInput,
};
use super::results::{
    cypher_result, search_hits, CompressionOptions, SQLNotice, SQLResult, SearchHit,
};
use super::tasks::{
    CalibrationReportTask, EstimateScoringParamsTask, HybridSearchTask, KNNSearchTask,
    LearnScoringParamsTask, RobustHybridSearchTask, RunCypherTask, SQLBatchTask, SQLTask,
    SearchTask, VectorSimilarityTask,
};
use super::value::{document_from_js, JSValue};
use super::{
    napi, Arc, AsyncTask, BTreeMap, CoreEngine, CoreSQLParam, Either, Error, Float32Array,
    Function, Path, Result,
};

// ---------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------

#[napi]
pub struct Engine {
    inner: Option<Arc<CoreEngine>>,
}

impl Engine {
    fn inner(&self) -> Result<&Arc<CoreEngine>> {
        self.inner
            .as_ref()
            .ok_or_else(|| Error::from_reason("engine is closed"))
    }
}

#[napi]
impl Engine {
    /// Create an in-memory engine.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            inner: Some(Arc::new(CoreEngine::new())),
        }
    }

    #[napi(factory)]
    pub fn open(path: String) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                CoreEngine::open(Path::new(&path)).map_err(runtime_error)?,
            )),
        })
    }

    /// Create an independent SQL session over this persistent database.
    #[napi]
    pub fn new_session(&self) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                self.inner()?.new_session().map_err(runtime_error)?,
            )),
        })
    }

    #[napi(factory)]
    pub fn open_encrypted(path: String, key: String) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                CoreEngine::open_encrypted(Path::new(&path), &key).map_err(runtime_error)?,
            )),
        })
    }

    #[napi(factory)]
    pub fn open_auto(path: String, key: Option<String>) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                CoreEngine::open_auto(Path::new(&path), key.as_deref()).map_err(runtime_error)?,
            )),
        })
    }

    #[napi(factory)]
    pub fn open_compressed(path: String, options: Option<CompressionOptions>) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                CoreEngine::open_compressed(Path::new(&path), compression_options(options)?)
                    .map_err(runtime_error)?,
            )),
        })
    }

    #[napi(factory)]
    pub fn open_compressed_encrypted(
        path: String,
        key: String,
        options: Option<CompressionOptions>,
    ) -> Result<Engine> {
        Ok(Engine {
            inner: Some(Arc::new(
                CoreEngine::open_compressed_encrypted(
                    Path::new(&path),
                    &key,
                    compression_options(options)?,
                )
                .map_err(runtime_error)?,
            )),
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
            engine: self.inner()?.clone(),
            query,
            params: params_from_input(params)?,
        }))
    }

    #[napi]
    pub fn sql_sync(&self, query: String, params: Option<Vec<ParamInput>>) -> Result<SQLResult> {
        let params = params_from_input(params)?;
        self.inner()?
            .sql(&query, &params)
            .map_err(runtime_error)?
            .try_into()
    }

    #[napi(ts_return_type = "Promise<Array<SQLResult>>")]
    pub fn sql_batch(
        &self,
        statements: Vec<(String, Vec<ParamInput>)>,
    ) -> Result<AsyncTask<SQLBatchTask>> {
        Ok(AsyncTask::new(SQLBatchTask {
            engine: self.inner()?.clone(),
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
        self.inner()?
            .sql_batch(&borrowed)
            .map_err(runtime_error)?
            .into_iter()
            .map(SQLResult::try_from)
            .collect()
    }

    #[napi]
    pub fn register_scalar_function(
        &self,
        name: String,
        #[napi(ts_arg_type = "(...args: JSValue[]) => JSValue")] callback: Function<
            '_,
            CallbackArguments,
            SynchronousJSValue,
        >,
        options: Option<JSFunctionOptions>,
    ) -> Result<()> {
        let function = JSScalarFunction::new(name.clone(), callback)?;
        self.inner()?
            .register_scalar_function_with_options(&name, function_options(options), function)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn register_table_function(
        &self,
        name: String,
        #[napi(
            ts_arg_type = "(...args: JSValue[]) => SQLTableFunctionResult | Array<Record<string, JSValue>>"
        )]
        callback: Function<'_, CallbackArguments, SynchronousJSValue>,
        options: Option<JSFunctionOptions>,
    ) -> Result<()> {
        let function = JSTableFunction::new(name.clone(), callback)?;
        self.inner()?
            .register_table_function_with_options(&name, function_options(options), function)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn register_aggregate_function(
        &self,
        name: String,
        #[napi(ts_arg_type = "() => SQLAggregateState")] factory: Function<
            '_,
            CallbackArguments,
            JSAggregateCallbacks,
        >,
        options: Option<JSFunctionOptions>,
    ) -> Result<()> {
        let function = JSAggregateFunction::new(name.clone(), factory)?;
        self.inner()?
            .register_aggregate_function_with_options(&name, function_options(options), function)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn create_default_table(&self, name: String, fts_fields: Vec<String>) -> Result<()> {
        self.inner()?
            .create_default_table(&name, fts_fields)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn create_vector_field(
        &self,
        table: String,
        field: String,
        dimensions: u32,
    ) -> Result<bool> {
        self.inner()?
            .create_vector_field(&table, &field, dimensions)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn add_document(
        &self,
        table: String,
        doc_id: i64,
        document: BTreeMap<String, JSValue>,
    ) -> Result<()> {
        self.inner()?
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
                        vec![vector_from_f64(single, &format!("vector field `{field}`"))?]
                    }
                    Either::B(rows) => tensor_from_f64(rows, &format!("vector field `{field}`"))?,
                };
                Ok((field, rows))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.inner()?
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
        self.inner()?
            .add_vector(
                &table,
                doc_id_from_input(doc_id)?,
                &field,
                vector_from_input(vector, &format!("vector field `{field}`"))?,
            )
            .map_err(runtime_error)
    }

    #[napi]
    pub fn add_vector_values(
        &self,
        table: String,
        doc_id: i64,
        field: String,
        vectors: Vec<Vec<f64>>,
    ) -> Result<bool> {
        self.inner()?
            .add_vector_values(
                &table,
                doc_id_from_input(doc_id)?,
                &field,
                tensor_from_f64(vectors, &format!("vector field `{field}`"))?,
            )
            .map_err(runtime_error)
    }

    #[napi]
    pub fn get_document(
        &self,
        table: String,
        doc_id: i64,
    ) -> Result<Option<BTreeMap<String, JSValue>>> {
        Ok(self
            .inner()?
            .get_document(&table, doc_id_from_input(doc_id)?)
            .map_err(runtime_error)?
            .map(|document| {
                document
                    .into_iter()
                    .map(|(key, value)| (key, JSValue(value)))
                    .collect()
            }))
    }

    #[napi]
    pub fn delete_document(&self, table: String, doc_id: i64) -> Result<()> {
        self.inner()?
            .delete_document(&table, doc_id_from_input(doc_id)?)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn document_count(&self, table: String) -> Result<i64> {
        js_number_from_u64(
            self.inner()?
                .document_count(&table)
                .map_err(runtime_error)?,
            "document count",
        )
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn search(
        &self,
        table: String,
        field: String,
        query: String,
        top_k: Option<u32>,
        scoring: Option<String>,
    ) -> Result<AsyncTask<SearchTask>> {
        Ok(AsyncTask::new(SearchTask {
            engine: self.inner()?.clone(),
            table,
            field,
            query,
            top_k: usize_from_u32(top_k.unwrap_or(10), "topK")?,
            scoring: scoring.unwrap_or_else(|| "bm25".to_string()),
        }))
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
        let mode = scoring_mode(self.inner()?, &table, &field, &scoring)?;
        search_hits(
            self.inner()?
                .search(
                    &table,
                    &field,
                    &query,
                    &mode,
                    usize_from_u32(top_k.unwrap_or(10), "topK")?,
                )
                .map_err(runtime_error)?,
        )
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn knn_search(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
    ) -> Result<AsyncTask<KNNSearchTask>> {
        Ok(AsyncTask::new(KNNSearchTask {
            engine: self.inner()?.clone(),
            table,
            field,
            vector: vector_from_input(vector, "KNN query vector")?,
            top_k: usize_from_u32(top_k.unwrap_or(10), "topK")?,
        }))
    }

    #[napi]
    pub fn knn_search_sync(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchHit>> {
        search_hits(
            self.inner()?
                .knn_search(
                    &table,
                    &field,
                    vector_from_input(vector, "KNN query vector")?,
                    usize_from_u32(top_k.unwrap_or(10), "topK")?,
                )
                .map_err(runtime_error)?,
        )
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    pub fn vector_similarity_search(
        &self,
        table: String,
        field: String,
        vector: Either<Float32Array, Vec<f64>>,
        threshold: f64,
    ) -> Result<AsyncTask<VectorSimilarityTask>> {
        Ok(AsyncTask::new(VectorSimilarityTask {
            engine: self.inner()?.clone(),
            table,
            field,
            vector: vector_from_input(vector, "vector-similarity query vector")?,
            threshold: f32_from_f64(threshold, "vector-similarity threshold")?,
        }))
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
    ) -> Result<AsyncTask<HybridSearchTask>> {
        let top_k = usize_from_u32(top_k.unwrap_or(10), "topK")?;
        let knn_pool = match knn_pool {
            Some(pool) => usize_from_u32(pool, "knnPool")?,
            None => top_k.checked_mul(4).ok_or_else(|| {
                Error::from_reason("default knnPool overflows the platform usize range")
            })?,
        };
        Ok(AsyncTask::new(HybridSearchTask {
            engine: self.inner()?.clone(),
            table,
            text_field,
            text_query,
            vector_field,
            query_vector: vector_from_input(query_vector, "hybrid query vector")?,
            top_k,
            knn_pool,
        }))
    }

    #[napi(ts_return_type = "Promise<Array<SearchHit>>")]
    #[allow(clippy::too_many_arguments)]
    pub fn robust_hybrid_search(
        &self,
        table: String,
        text_field: String,
        text_query: String,
        vector_field: String,
        query_vector: Either<Float32Array, Vec<f64>>,
        top_k: Option<u32>,
        knn_pool: Option<u32>,
        alpha: Option<f64>,
    ) -> Result<AsyncTask<RobustHybridSearchTask>> {
        let top_k = usize_from_u32(top_k.unwrap_or(10), "topK")?;
        let knn_pool = match knn_pool {
            Some(pool) => usize_from_u32(pool, "knnPool")?,
            None => top_k.checked_mul(4).ok_or_else(|| {
                Error::from_reason("default knnPool overflows the platform usize range")
            })?,
        };
        let alpha = alpha.unwrap_or(0.5);
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(Error::from_reason("alpha must be finite and in [0, 1]"));
        }
        Ok(AsyncTask::new(RobustHybridSearchTask {
            engine: self.inner()?.clone(),
            table,
            text_field,
            text_query,
            vector_field,
            query_vector: vector_from_input(query_vector, "robust hybrid query vector")?,
            top_k,
            knn_pool,
            alpha,
        }))
    }

    #[napi(ts_return_type = "Promise<Record<string, number>>")]
    pub fn estimate_scoring_params(
        &self,
        table: String,
        field: String,
        n_samples: Option<u32>,
        tokens_per_query: Option<u32>,
        seed: Option<i64>,
    ) -> Result<AsyncTask<EstimateScoringParamsTask>> {
        Ok(AsyncTask::new(EstimateScoringParamsTask {
            engine: self.inner()?.clone(),
            table,
            field,
            n_samples: usize_from_u32(n_samples.unwrap_or(50), "nSamples")?,
            tokens_per_query: usize_from_u32(tokens_per_query.unwrap_or(5), "tokensPerQuery")?,
            seed: seed.unwrap_or(42),
        }))
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
            engine: self.inner()?.clone(),
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
        self.inner()?
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
            engine: self.inner()?.clone(),
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
        self.inner()?
            .save_scoring_params(&name, &json)
            .map_err(runtime_error)
    }

    #[napi]
    pub fn load_scoring_params(&self, name: String) -> Result<Option<BTreeMap<String, f64>>> {
        self.inner()?
            .load_scoring_params(&name)
            .map_err(runtime_error)?
            .map(|json| parse_scoring_params(&name, &json))
            .transpose()
    }

    #[napi]
    pub fn load_all_scoring_params(&self) -> Result<BTreeMap<String, BTreeMap<String, f64>>> {
        self.inner()?
            .load_all_scoring_params()
            .map_err(runtime_error)?
            .into_iter()
            .map(|(name, json)| {
                let params = parse_scoring_params(&name, &json)?;
                Ok((name, params))
            })
            .collect()
    }

    #[napi]
    pub fn drop_scoring_params(&self, name: String) -> Result<bool> {
        self.inner()?
            .drop_scoring_params(&name)
            .map_err(runtime_error)
    }

    #[napi(ts_return_type = "Promise<SQLResult>")]
    pub fn run_cypher(
        &self,
        graph: String,
        query: String,
        params: Option<BTreeMap<String, JSValue>>,
    ) -> Result<AsyncTask<RunCypherTask>> {
        Ok(AsyncTask::new(RunCypherTask {
            engine: self.inner()?.clone(),
            graph,
            query,
            params: params.map(document_from_js).unwrap_or_default(),
        }))
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
            .inner()?
            .run_cypher(&graph, &query, params)
            .map_err(runtime_error)?;
        Ok(cypher_result(columns, rows))
    }

    #[napi]
    pub fn create_graph(&self, name: String) -> Result<bool> {
        self.inner()?.create_graph(&name).map_err(runtime_error)
    }

    #[napi]
    pub fn drop_graph(&self, name: String) -> Result<bool> {
        self.inner()?.drop_graph(&name).map_err(runtime_error)
    }

    #[napi]
    pub fn list_graphs(&self) -> Result<Vec<String>> {
        self.inner()?.list_graphs().map_err(runtime_error)
    }

    #[napi]
    pub fn list_path_indexes(&self) -> Result<Vec<String>> {
        self.inner()?.list_path_indexes().map_err(runtime_error)
    }

    #[napi]
    pub fn table_names(&self) -> Result<Vec<String>> {
        self.inner()?.table_names().map_err(runtime_error)
    }

    #[napi]
    pub fn list_views(&self) -> Result<Vec<String>> {
        self.inner()?.list_views().map_err(runtime_error)
    }

    #[napi]
    pub fn list_schemas(&self) -> Result<Vec<String>> {
        self.inner()?.list_schemas().map_err(runtime_error)
    }

    #[napi]
    pub fn list_sequences(&self) -> Result<Vec<String>> {
        self.inner()?
            .list_sequences()
            .map_err(|err| Error::from_reason(format!("list sequences: {err}")))
    }

    #[napi]
    pub fn list_named_analyzers(&self) -> Result<Vec<String>> {
        self.inner()?.list_named_analyzers().map_err(runtime_error)
    }

    #[napi]
    pub fn list_foreign_servers(&self) -> Result<Vec<String>> {
        self.inner()?.list_foreign_servers().map_err(runtime_error)
    }

    #[napi]
    pub fn list_foreign_tables(&self) -> Result<Vec<String>> {
        self.inner()?.list_foreign_tables().map_err(runtime_error)
    }

    #[napi(js_name = "takeSQLNotices")]
    pub fn take_sql_notices(&self) -> Result<Vec<SQLNotice>> {
        Ok(self
            .inner()?
            .take_sql_notices()
            .into_iter()
            .map(|(level, message)| SQLNotice { level, message })
            .collect())
    }

    #[napi(js_name = "sqlFunctionDepthLimit")]
    pub fn sql_function_depth_limit(&self) -> Result<u32> {
        u32::try_from(self.inner()?.sql_function_depth_limit()).map_err(|_| {
            Error::from_reason("SQL function depth limit exceeds the Node.js u32 bridge")
        })
    }

    #[napi(js_name = "setSQLFunctionDepthLimit")]
    pub fn set_sql_function_depth_limit(&self, limit: u32) -> Result<()> {
        self.inner()?
            .set_sql_function_depth_limit(usize_from_u32(limit, "SQL function depth limit")?);
        Ok(())
    }

    #[napi]
    pub fn cancel(&self) -> Result<()> {
        self.inner()?.cancel();
        Ok(())
    }

    #[napi]
    pub fn close(&mut self) -> Result<()> {
        let Some(inner) = self.inner.take() else {
            return Ok(());
        };
        if let Err(error) = inner.close() {
            self.inner = Some(inner);
            return Err(Error::from_reason(format!("close engine: {error}")));
        }
        Ok(())
    }
}
