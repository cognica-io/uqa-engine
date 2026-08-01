//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapters from Python callables to SQL UDF contracts.

use super::{
    py_error_to_sql, table_function_result_from_py, value_from_py, values_to_py_tuple, Py, PyAny,
    PyAnyMethods, Python, SQLAggregateFunction, SQLAggregateState, SQLError, SQLScalarFunction,
    SQLTableFunction, SQLTableFunctionResult, Value,
};

pub(super) struct PyScalarFunction {
    pub(super) name: String,
    pub(super) callable: Py<PyAny>,
}

impl SQLScalarFunction for PyScalarFunction {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        Python::attach(|py| {
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare scalar args", err))?;
            let result = self
                .callable
                .bind(py)
                .call1(py_args)
                .map_err(|err| py_error_to_sql(py, &self.name, "call scalar function", err))?;
            value_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert scalar result", err))
        })
    }
}

pub(super) struct PyTableFunction {
    pub(super) name: String,
    pub(super) callable: Py<PyAny>,
}

impl SQLTableFunction for PyTableFunction {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        Python::attach(|py| {
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare table args", err))?;
            let result = self
                .callable
                .bind(py)
                .call1(py_args)
                .map_err(|err| py_error_to_sql(py, &self.name, "call table function", err))?;
            table_function_result_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert table result", err))
        })
    }
}

pub(super) struct PyAggregateFunction {
    pub(super) name: String,
    pub(super) factory: Py<PyAny>,
}

impl SQLAggregateFunction for PyAggregateFunction {
    fn create_state(&self) -> Box<dyn SQLAggregateState> {
        Python::attach(|py| match self.factory.bind(py).call0() {
            Ok(state) => Box::new(PyAggregateStateWrapper {
                name: self.name.clone(),
                state: Some(state.unbind()),
                init_error: None,
            }),
            Err(err) => Box::new(PyAggregateStateWrapper {
                name: self.name.clone(),
                state: None,
                init_error: Some(py_error_to_sql(
                    py,
                    &self.name,
                    "create aggregate state",
                    err,
                )),
            }),
        })
    }
}

struct PyAggregateStateWrapper {
    pub(super) name: String,
    state: Option<Py<PyAny>>,
    init_error: Option<SQLError>,
}

impl SQLAggregateState for PyAggregateStateWrapper {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        if let Some(err) = self.init_error.take() {
            return Err(err);
        }
        let Some(state) = self.state.as_ref() else {
            return Err(SQLError::Internal(format!(
                "Python aggregate `{}` has no state",
                self.name
            )));
        };
        Python::attach(|py| {
            let method = state
                .bind(py)
                .getattr("observe")
                .or_else(|_| state.bind(py).getattr("step"))
                .map_err(|err| py_error_to_sql(py, &self.name, "find aggregate observe", err))?;
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare aggregate args", err))?;
            method
                .call1(py_args)
                .map(|_| ())
                .map_err(|err| py_error_to_sql(py, &self.name, "observe aggregate value", err))
        })
    }

    fn finish(&self) -> Result<Value, SQLError> {
        if let Some(err) = self.init_error.as_ref() {
            return Err(SQLError::Internal(err.to_string()));
        }
        let Some(state) = self.state.as_ref() else {
            return Err(SQLError::Internal(format!(
                "Python aggregate `{}` has no state",
                self.name
            )));
        };
        Python::attach(|py| {
            let method = state
                .bind(py)
                .getattr("finish")
                .or_else(|_| state.bind(py).getattr("finalize"))
                .map_err(|err| py_error_to_sql(py, &self.name, "find aggregate finish", err))?;
            let result = method
                .call0()
                .map_err(|err| py_error_to_sql(py, &self.name, "finish aggregate", err))?;
            value_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert aggregate result", err))
        })
    }
}
