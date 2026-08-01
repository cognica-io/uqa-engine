//! Scoring, search, and migration report conversion to Python.

use super::{
    BTreeMap, CalibrationReport, Py, PyAny, PyDict, PyDictMethods, PyList, PyListMethods, PyResult,
    PyValueError, Python, PythonMigrationReport, ScoredEntry,
};

pub(super) fn parse_scoring_params(name: &str, json: &str) -> PyResult<BTreeMap<String, f64>> {
    serde_json::from_str(json).map_err(|err| {
        PyValueError::new_err(format!(
            "scoring params `{name}` are not a map of floats: {err}"
        ))
    })
}

pub(super) fn calibration_report_to_py(
    py: Python<'_>,
    report: &CalibrationReport,
) -> PyResult<Py<PyAny>> {
    let bins = PyList::empty(py);
    for bin in &report.bins {
        let entry = PyDict::new(py);
        entry.set_item("avg_predicted", bin.avg_predicted)?;
        entry.set_item("avg_actual", bin.avg_actual)?;
        entry.set_item("count", bin.count)?;
        bins.append(entry)?;
    }
    let dict = PyDict::new(py);
    dict.set_item("ece", report.ece)?;
    dict.set_item("brier", report.brier)?;
    dict.set_item("log_loss", report.log_loss)?;
    dict.set_item("bins", bins)?;
    Ok(dict.into_any().unbind())
}

pub(super) fn scored_entries_to_py(py: Python<'_>, entries: &[ScoredEntry]) -> PyResult<Py<PyAny>> {
    let list = PyList::empty(py);
    for entry in entries {
        let dict = PyDict::new(py);
        dict.set_item("doc_id", entry.doc_id)?;
        dict.set_item("score", entry.score)?;
        list.append(dict)?;
    }
    Ok(list.into_any().unbind())
}

pub(super) fn migration_report_to_py(
    py: Python<'_>,
    report: &PythonMigrationReport,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("source_path", report.source_path.to_string_lossy().as_ref())?;
    dict.set_item(
        "destination_path",
        report.destination_path.to_string_lossy().as_ref(),
    )?;
    dict.set_item("tables", report.tables)?;
    dict.set_item("documents", report.documents)?;
    dict.set_item("fts_fields", report.fts_fields)?;
    dict.set_item("vector_fields", report.vector_fields)?;
    dict.set_item("indexes", report.indexes)?;
    dict.set_item("analyzers", report.analyzers)?;
    dict.set_item("table_field_analyzers", report.table_field_analyzers)?;
    dict.set_item("foreign_servers", report.foreign_servers)?;
    dict.set_item("foreign_tables", report.foreign_tables)?;
    dict.set_item("graphs", report.graphs)?;
    dict.set_item("graph_vertices", report.graph_vertices)?;
    dict.set_item("graph_edges", report.graph_edges)?;
    dict.set_item("path_indexes", report.path_indexes)?;
    dict.set_item("scoring_params", report.scoring_params)?;
    dict.set_item("models", report.models)?;
    dict.set_item("column_stats", report.column_stats)?;
    Ok(dict.into_any().unbind())
}
