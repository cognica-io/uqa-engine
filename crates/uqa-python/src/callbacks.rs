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
        Python::attach(|py| {
            let methods = self
                .factory
                .bind(py)
                .call0()
                .map_err(|err| py_error_to_sql(py, &self.name, "create aggregate state", err))
                .map(|state| {
                    let method = |name, alternate, action| {
                        state
                            .getattr(name)
                            .or_else(|_| state.getattr(alternate))
                            .map(pyo3::Bound::unbind)
                            .map_err(|err| py_error_to_sql(py, &self.name, action, err))
                    };
                    AggregateMethods {
                        observe: method("observe", "step", "find aggregate observe"),
                        finish: method("finish", "finalize", "find aggregate finish"),
                        _state: state.unbind(),
                    }
                });
            Box::new(PyAggregateStateWrapper {
                name: self.name.clone(),
                methods,
            }) as Box<dyn SQLAggregateState>
        })
    }
}

struct AggregateMethods {
    observe: Result<Py<PyAny>, SQLError>,
    finish: Result<Py<PyAny>, SQLError>,
    // Static methods and callable attributes may not retain the factory's state object.
    _state: Py<PyAny>,
}

struct PyAggregateStateWrapper {
    name: String,
    methods: Result<AggregateMethods, SQLError>,
}

impl SQLAggregateState for PyAggregateStateWrapper {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let methods = self.methods.as_ref().map_err(Clone::clone)?;
        let observe = methods.observe.as_ref().map_err(Clone::clone)?;
        Python::attach(|py| {
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare aggregate args", err))?;
            observe
                .bind(py)
                .call1(py_args)
                .map(|_| ())
                .map_err(|err| py_error_to_sql(py, &self.name, "observe aggregate value", err))
        })
    }

    fn finish(&self) -> Result<Value, SQLError> {
        let methods = self
            .methods
            .as_ref()
            .map_err(|err| SQLError::Internal(err.to_string()))?;
        let finish = methods.finish.as_ref().map_err(Clone::clone)?;
        Python::attach(|py| {
            let result = finish
                .bind(py)
                .call0()
                .map_err(|err| py_error_to_sql(py, &self.name, "finish aggregate", err))?;
            value_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert aggregate result", err))
        })
    }
}
