//! Python-database migration entry point.

use super::{
    migrate_python_database, migration_report_to_py, pyfunction, runtime_error, PathBuf, Py, PyAny,
    PyResult, Python,
};

#[pyfunction]
pub(super) fn migrate_python_db(source: PathBuf, destination: PathBuf) -> PyResult<Py<PyAny>> {
    Python::attach(|py| {
        let report = migrate_python_database(&source, &destination).map_err(runtime_error)?;
        migration_report_to_py(py, &report)
    })
}
