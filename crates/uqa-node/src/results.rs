//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node-API result objects and SQL parameter wrappers.

use super::input::{js_number_from_u64, js_number_from_usize, tensor_from_f64, vector_from_input};
use super::value::{value_from_unknown, JSValue};
use super::{
    napi, BTreeMap, CoreCalibrationReport, CoreSQLParam, CoreSQLResult, Either, Error,
    Float32Array, PythonMigrationReport, Result, ScoredEntry, Unknown, Value,
};

// ---------------------------------------------------------------------
// Result objects
// ---------------------------------------------------------------------

#[napi(object, js_name = "SQLResult")]
pub struct SQLResult {
    pub columns: Vec<String>,
    pub rows: Vec<BTreeMap<String, JSValue>>,
    pub affected_rows: i64,
}

impl TryFrom<CoreSQLResult> for SQLResult {
    type Error = Error;

    fn try_from(result: CoreSQLResult) -> Result<Self> {
        Ok(Self {
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
            affected_rows: js_number_from_u64(result.affected_rows, "affected row count")?,
        })
    }
}

pub(super) fn cypher_result(columns: Vec<String>, rows: Vec<BTreeMap<String, Value>>) -> SQLResult {
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

pub(super) fn search_hits(entries: Vec<ScoredEntry>) -> Result<Vec<SearchHit>> {
    entries
        .into_iter()
        .map(|entry| {
            Ok(SearchHit {
                doc_id: js_number_from_u64(entry.doc_id, "search result document ID")?,
                score: entry.score,
            })
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

impl TryFrom<CoreCalibrationReport> for CalibrationReport {
    type Error = Error;

    fn try_from(report: CoreCalibrationReport) -> Result<Self> {
        Ok(Self {
            ece: report.ece,
            brier: report.brier,
            log_loss: report.log_loss,
            bins: report
                .bins
                .into_iter()
                .map(|bin| {
                    Ok(ReliabilityBin {
                        avg_predicted: bin.avg_predicted,
                        avg_actual: bin.avg_actual,
                        count: js_number_from_usize(bin.count, "calibration bin count")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
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

impl TryFrom<PythonMigrationReport> for MigrationReport {
    type Error = Error;

    fn try_from(report: PythonMigrationReport) -> Result<Self> {
        Ok(Self {
            source_path: report.source_path.to_string_lossy().into_owned(),
            destination_path: report.destination_path.to_string_lossy().into_owned(),
            tables: js_number_from_usize(report.tables, "migrated table count")?,
            documents: js_number_from_usize(report.documents, "migrated document count")?,
            fts_fields: js_number_from_usize(report.fts_fields, "migrated FTS field count")?,
            vector_fields: js_number_from_usize(
                report.vector_fields,
                "migrated vector field count",
            )?,
            indexes: js_number_from_usize(report.indexes, "migrated index count")?,
            analyzers: js_number_from_usize(report.analyzers, "migrated analyzer count")?,
            table_field_analyzers: js_number_from_usize(
                report.table_field_analyzers,
                "migrated table-field analyzer count",
            )?,
            foreign_servers: js_number_from_usize(
                report.foreign_servers,
                "migrated foreign server count",
            )?,
            foreign_tables: js_number_from_usize(
                report.foreign_tables,
                "migrated foreign table count",
            )?,
            graphs: js_number_from_usize(report.graphs, "migrated graph count")?,
            graph_vertices: js_number_from_usize(
                report.graph_vertices,
                "migrated graph vertex count",
            )?,
            graph_edges: js_number_from_usize(report.graph_edges, "migrated graph edge count")?,
            path_indexes: js_number_from_usize(report.path_indexes, "migrated path index count")?,
            scoring_params: js_number_from_usize(
                report.scoring_params,
                "migrated scoring parameter count",
            )?,
            models: js_number_from_usize(report.models, "migrated model count")?,
            column_stats: js_number_from_usize(
                report.column_stats,
                "migrated column-statistics count",
            )?,
        })
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
    pub(super) inner: CoreSQLParam,
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
    pub fn vector(values: Either<Float32Array, Vec<f64>>) -> Result<SQLParam> {
        Ok(Self {
            inner: CoreSQLParam::vector(vector_from_input(values, "SQL vector parameter")?),
        })
    }

    #[napi(factory)]
    pub fn tensor(values: Vec<Vec<f64>>) -> Result<SQLParam> {
        Ok(Self {
            inner: CoreSQLParam::tensor(tensor_from_f64(values, "SQL tensor parameter")?),
        })
    }
}
