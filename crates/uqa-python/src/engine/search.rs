//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, vector, and hybrid search APIs.

use super::{
    pymethods, runtime_error, scored_entries_to_py, scoring_mode, validate_vector,
    HybridSearchParams, Py, PyAny, PyEngine, PyResult, PyValueError, Python,
    RobustHybridSearchParams, ScoredEntry,
};

#[pymethods]
impl PyEngine {
    #[pyo3(signature = (table, field, query, top_k=10, scoring="bm25"))]
    fn search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        query: &str,
        top_k: usize,
        scoring: &str,
    ) -> PyResult<Py<PyAny>> {
        let inner = self.inner()?;
        let entries = py.detach(|| -> PyResult<Vec<ScoredEntry>> {
            let mode = scoring_mode(inner, table, field, scoring)?;
            inner
                .search(table, field, query, &mode, top_k)
                .map_err(runtime_error)
        })?;
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (table, field, vector, top_k=10))]
    fn knn_search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        vector: Vec<f32>,
        top_k: usize,
    ) -> PyResult<Py<PyAny>> {
        let vector = validate_vector(vector, "KNN query vector")?;
        let inner = self.inner()?;
        let entries = py
            .detach(|| inner.knn_search(table, field, vector, top_k))
            .map_err(runtime_error)?;
        scored_entries_to_py(py, &entries)
    }

    fn vector_similarity_search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        vector: Vec<f32>,
        threshold: f32,
    ) -> PyResult<Py<PyAny>> {
        let vector = validate_vector(vector, "vector-similarity query vector")?;
        if !threshold.is_finite() {
            return Err(PyValueError::new_err(
                "vector-similarity threshold must be finite",
            ));
        }
        let inner = self.inner()?;
        let entries = py
            .detach(|| inner.vector_similarity_search(table, field, vector, threshold))
            .map_err(runtime_error)?;
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (table, text_field, text_query, vector_field, query_vector, top_k=10, knn_pool=None))]
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the stable binding signature"
    )]
    fn hybrid_search(
        &self,
        py: Python<'_>,
        table: &str,
        text_field: &str,
        text_query: &str,
        vector_field: &str,
        query_vector: Vec<f32>,
        top_k: usize,
        knn_pool: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let query_vector = validate_vector(query_vector, "hybrid query vector")?;
        let knn_pool = match knn_pool {
            Some(pool) => pool,
            None => top_k.checked_mul(4).ok_or_else(|| {
                PyValueError::new_err("default knn_pool exceeds the platform usize range")
            })?,
        };
        let params = HybridSearchParams {
            table,
            text_field,
            text_query,
            vector_field,
            query_vector,
            knn_pool,
            top_k,
        };
        let inner = self.inner()?;
        let entries = py
            .detach(|| inner.hybrid_search(&params))
            .map_err(runtime_error)?;
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (table, text_field, text_query, vector_field, query_vector, top_k=10, knn_pool=None, alpha=0.5))]
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the stable binding signature"
    )]
    fn robust_hybrid_search(
        &self,
        py: Python<'_>,
        table: &str,
        text_field: &str,
        text_query: &str,
        vector_field: &str,
        query_vector: Vec<f32>,
        top_k: usize,
        knn_pool: Option<usize>,
        alpha: f64,
    ) -> PyResult<Py<PyAny>> {
        let query_vector = validate_vector(query_vector, "robust hybrid query vector")?;
        let knn_pool = match knn_pool {
            Some(pool) => pool,
            None => top_k.checked_mul(4).ok_or_else(|| {
                PyValueError::new_err("default knn_pool exceeds the platform usize range")
            })?,
        };
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(PyValueError::new_err("alpha must be finite and in [0, 1]"));
        }
        let params = RobustHybridSearchParams {
            table,
            text_field,
            text_query,
            vector_field,
            query_vector,
            knn_pool,
            alpha,
            top_k,
        };
        let inner = self.inner()?;
        let entries = py
            .detach(|| inner.robust_hybrid_search(&params))
            .map_err(runtime_error)?;
        scored_entries_to_py(py, &entries)
    }
}
