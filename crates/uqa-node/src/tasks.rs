//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Asynchronous Node-API tasks executed on the libuv worker pool.

use super::input::{runtime_error, scoring_mode};
use super::results::{cypher_result, search_hits, CalibrationReport, SQLResult, SearchHit};
use super::{
    Arc, BTreeMap, CoreCalibrationReport, CoreEngine, CoreSQLParam, CoreSQLResult, Env,
    HybridSearchParams, Result, RobustHybridSearchParams, ScoredEntry, Task, Value,
};

// ---------------------------------------------------------------------
// Async tasks
// ---------------------------------------------------------------------

pub struct SQLTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) query: String,
    pub(super) params: Vec<CoreSQLParam>,
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
        output.try_into()
    }
}

pub struct SQLBatchTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) statements: Vec<(String, Vec<CoreSQLParam>)>,
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
        output.into_iter().map(SQLResult::try_from).collect()
    }
}

pub struct SearchTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) query: String,
    pub(super) top_k: usize,
    pub(super) scoring: String,
}

impl Task for SearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        let mode = scoring_mode(&self.engine, &self.table, &self.field, &self.scoring)?;
        self.engine
            .search(&self.table, &self.field, &self.query, &mode, self.top_k)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        search_hits(output)
    }
}

pub struct KNNSearchTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) vector: Vec<f32>,
    pub(super) top_k: usize,
}

impl Task for KNNSearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .knn_search(&self.table, &self.field, &self.vector, self.top_k)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        search_hits(output)
    }
}

pub struct VectorSimilarityTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) vector: Vec<f32>,
    pub(super) threshold: f32,
}

impl Task for VectorSimilarityTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.engine
            .vector_similarity_search(
                &self.table,
                &self.field,
                self.vector.clone(),
                self.threshold,
            )
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        search_hits(output)
    }
}

pub struct HybridSearchTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) text_field: String,
    pub(super) text_query: String,
    pub(super) vector_field: String,
    pub(super) query_vector: Vec<f32>,
    pub(super) top_k: usize,
    pub(super) knn_pool: usize,
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
            top_k: self.top_k,
        };
        self.engine.hybrid_search(&params).map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        search_hits(output)
    }
}

pub struct RobustHybridSearchTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) text_field: String,
    pub(super) text_query: String,
    pub(super) vector_field: String,
    pub(super) query_vector: Vec<f32>,
    pub(super) top_k: usize,
    pub(super) knn_pool: usize,
    pub(super) alpha: f64,
}

impl Task for RobustHybridSearchTask {
    type Output = Vec<ScoredEntry>;
    type JsValue = Vec<SearchHit>;

    fn compute(&mut self) -> Result<Self::Output> {
        let params = RobustHybridSearchParams {
            table: &self.table,
            text_field: &self.text_field,
            text_query: &self.text_query,
            vector_field: &self.vector_field,
            query_vector: self.query_vector.clone(),
            knn_pool: self.knn_pool,
            alpha: self.alpha,
            top_k: self.top_k,
        };
        self.engine
            .robust_hybrid_search(&params)
            .map_err(runtime_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        search_hits(output)
    }
}

pub struct RunCypherTask {
    pub(super) engine: Arc<CoreEngine>,
    pub(super) graph: String,
    pub(super) query: String,
    pub(super) params: BTreeMap<String, Value>,
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
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) n_samples: usize,
    pub(super) tokens_per_query: usize,
    pub(super) seed: i64,
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
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) query: String,
    pub(super) labels: Vec<u8>,
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
    pub(super) engine: Arc<CoreEngine>,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) query: String,
    pub(super) labels: Vec<u8>,
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
        output.try_into()
    }
}
