//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table, document, and vector mutation APIs.

use super::{
    document_from_py, map_to_py, pymethods, runtime_error, validate_tensor, validate_vector,
    vector_values_from_py, Bound, Py, PyAny, PyEngine, PyResult, Python,
};

#[pymethods]
impl PyEngine {
    fn create_default_table(&self, name: &str, fts_fields: Vec<String>) -> PyResult<()> {
        self.inner()?
            .create_default_table(name, fts_fields)
            .map_err(runtime_error)
    }

    fn create_vector_field(&self, table: &str, field: &str, dimensions: u32) -> PyResult<bool> {
        self.inner()?
            .create_vector_field(table, field, dimensions)
            .map_err(runtime_error)
    }

    fn add_document(&self, table: &str, doc_id: u64, document: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner()?
            .add_document(table, doc_id, document_from_py(document)?)
            .map_err(runtime_error)
    }

    fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: u64,
        document: &Bound<'_, PyAny>,
        vectors: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.inner()?
            .add_document_with_vector_values(
                table,
                doc_id,
                document_from_py(document)?,
                vector_values_from_py(vectors)?,
            )
            .map_err(runtime_error)?;
        Ok(())
    }

    fn add_vector(
        &self,
        table: &str,
        doc_id: u64,
        field: &str,
        vector: Vec<f32>,
    ) -> PyResult<bool> {
        self.inner()?
            .add_vector(
                table,
                doc_id,
                field,
                validate_vector(vector, &format!("vector field `{field}`"))?,
            )
            .map_err(runtime_error)
    }

    fn add_vector_values(
        &self,
        table: &str,
        doc_id: u64,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> PyResult<bool> {
        self.inner()?
            .add_vector_values(
                table,
                doc_id,
                field,
                validate_tensor(vectors, &format!("vector field `{field}`"))?,
            )
            .map_err(runtime_error)
    }

    fn get_document(&self, py: Python<'_>, table: &str, doc_id: u64) -> PyResult<Py<PyAny>> {
        match self
            .inner()?
            .get_document(table, doc_id)
            .map_err(runtime_error)?
        {
            Some(document) => map_to_py(py, &document),
            None => Ok(py.None()),
        }
    }

    fn delete_document(&self, table: &str, doc_id: u64) -> PyResult<()> {
        self.inner()?
            .delete_document(table, doc_id)
            .map_err(runtime_error)
    }

    fn document_count(&self, table: &str) -> PyResult<u64> {
        self.inner()?.document_count(table).map_err(runtime_error)
    }
}
