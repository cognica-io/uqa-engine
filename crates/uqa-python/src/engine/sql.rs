//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL execution and Python UDF registration.

use super::{
    batch_from_py, ensure_callable, params_from_py, pymethods, runtime_error, Bound, Py,
    PyAggregateFunction, PyAny, PyEngine, PyResult, PySQLResult, PyScalarFunction, PyTableFunction,
    PyValueError, Python, SQLFunctionOptions, SQLFunctionVolatility, SQLParam,
};

#[pymethods]
impl PyEngine {
    #[pyo3(signature = (query, params=None))]
    fn sql(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        let query = query.to_string();
        let params = params_from_py(params)?;
        let inner = self.inner()?;
        let result = py
            .detach(|| inner.sql(&query, &params))
            .map_err(runtime_error)?;
        Ok(result.into())
    }

    #[pyo3(signature = (query, params=None))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        self.sql(py, query, params)
    }

    fn sql_batch(
        &self,
        py: Python<'_>,
        statements: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<PySQLResult>> {
        let statements = batch_from_py(statements)?;
        let borrowed: Vec<(&str, &[SQLParam])> = statements
            .iter()
            .map(|(sql, params)| (sql.as_str(), params.as_slice()))
            .collect();
        let inner = self.inner()?;
        let results = py
            .detach(|| inner.sql_batch(&borrowed))
            .map_err(runtime_error)?;
        Ok(results.into_iter().map(Into::into).collect())
    }

    #[pyo3(signature = (name, callable, *, volatility = "volatile", may_mutate_engine = true))]
    fn register_scalar_function(
        &self,
        py: Python<'_>,
        name: &str,
        callable: Py<PyAny>,
        volatility: &str,
        may_mutate_engine: bool,
    ) -> PyResult<()> {
        ensure_callable(py, &callable, "scalar function")?;
        self.inner()?
            .register_scalar_function_with_options(
                name,
                function_options(volatility, may_mutate_engine)?,
                PyScalarFunction {
                    name: name.to_string(),
                    callable,
                },
            )
            .map_err(runtime_error)
    }

    #[pyo3(signature = (name, callable, *, volatility = "volatile", may_mutate_engine = true))]
    fn register_table_function(
        &self,
        py: Python<'_>,
        name: &str,
        callable: Py<PyAny>,
        volatility: &str,
        may_mutate_engine: bool,
    ) -> PyResult<()> {
        ensure_callable(py, &callable, "table function")?;
        self.inner()?
            .register_table_function_with_options(
                name,
                function_options(volatility, may_mutate_engine)?,
                PyTableFunction {
                    name: name.to_string(),
                    callable,
                },
            )
            .map_err(runtime_error)
    }

    #[pyo3(signature = (name, factory, *, volatility = "volatile", may_mutate_engine = true))]
    fn register_aggregate_function(
        &self,
        py: Python<'_>,
        name: &str,
        factory: Py<PyAny>,
        volatility: &str,
        may_mutate_engine: bool,
    ) -> PyResult<()> {
        ensure_callable(py, &factory, "aggregate function factory")?;
        self.inner()?
            .register_aggregate_function_with_options(
                name,
                function_options(volatility, may_mutate_engine)?,
                PyAggregateFunction {
                    name: name.to_string(),
                    factory,
                },
            )
            .map_err(runtime_error)
    }
}

fn function_options(volatility: &str, may_mutate_engine: bool) -> PyResult<SQLFunctionOptions> {
    let volatility = match volatility.to_ascii_lowercase().as_str() {
        "volatile" => SQLFunctionVolatility::Volatile,
        "stable" => SQLFunctionVolatility::Stable,
        "immutable" => SQLFunctionVolatility::Immutable,
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown SQL function volatility `{other}`; expected volatile, stable, or immutable"
            )));
        }
    };
    Ok(SQLFunctionOptions::new(volatility, may_mutate_engine))
}
