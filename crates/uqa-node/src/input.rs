//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated JavaScript input conversion and engine option mapping.

use super::results::{CompressionOptions, SQLParam};
use super::value::value_from_unknown;
use super::{
    BM25Params, BTreeMap, ClassInstance, CoreEngine, CoreSQLParam, DatabaseFileFormat, Either,
    Error, Float32Array, Result, SQLiteCompressionOptions, ScoringMode, Unknown, MAX_SAFE_INTEGER,
};

pub(super) fn vector_from_input(
    values: Either<Float32Array, Vec<f64>>,
    context: &str,
) -> Result<Vec<f32>> {
    match values {
        Either::A(values) => values
            .iter()
            .copied()
            .enumerate()
            .map(|(index, value)| finite_f32(value, &format!("{context}[{index}]")))
            .collect(),
        Either::B(values) => vector_from_f64(values, context),
    }
}

pub(super) fn vector_from_f64(values: Vec<f64>, context: &str) -> Result<Vec<f32>> {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| f32_from_f64(value, &format!("{context}[{index}]")))
        .collect()
}

pub(super) fn tensor_from_f64(values: Vec<Vec<f64>>, context: &str) -> Result<Vec<Vec<f32>>> {
    values
        .into_iter()
        .enumerate()
        .map(|(row, values)| vector_from_f64(values, &format!("{context}[{row}]")))
        .collect()
}

pub(super) fn finite_f32(value: f32, context: &str) -> Result<f32> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::from_reason(format!(
            "{context} must be finite, got {value}"
        )))
    }
}

pub(super) fn f32_from_f64(value: f64, context: &str) -> Result<f32> {
    if !value.is_finite() {
        return Err(Error::from_reason(format!(
            "{context} must be finite, got {value}"
        )));
    }
    if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(Error::from_reason(format!(
            "{context} is outside the f32 range: {value}"
        )));
    }
    Ok(value as f32)
}

pub(super) fn usize_from_u32(value: u32, context: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| Error::from_reason(format!("{context} exceeds the platform usize range")))
}

pub(super) type ParamInput<'env> = Either<ClassInstance<'env, SQLParam>, Unknown<'env>>;

pub(super) fn params_from_input(params: Option<Vec<ParamInput<'_>>>) -> Result<Vec<CoreSQLParam>> {
    params
        .unwrap_or_default()
        .into_iter()
        .map(|param| match param {
            Either::A(instance) => Ok(instance.inner.clone()),
            Either::B(unknown) => Ok(CoreSQLParam::scalar(value_from_unknown(&unknown)?)),
        })
        .collect()
}

pub(super) fn labels_from_input(labels: Vec<u32>) -> Result<Vec<u8>> {
    labels
        .into_iter()
        .map(|label| {
            if label > 1 {
                Err(Error::from_reason("labels must contain only 0 or 1"))
            } else {
                u8::try_from(label).map_err(|_| Error::from_reason("label exceeds the u8 bridge"))
            }
        })
        .collect()
}

pub(super) fn js_number_from_u64(value: u64, label: &str) -> Result<i64> {
    if value > MAX_SAFE_INTEGER as u64 {
        return Err(Error::from_reason(format!(
            "{label} exceeds JavaScript's safe integer range"
        )));
    }
    i64::try_from(value).map_err(|_| Error::from_reason(format!("{label} exceeds the i64 bridge")))
}

pub(super) fn js_number_from_usize(value: usize, label: &str) -> Result<i64> {
    let value = u64::try_from(value)
        .map_err(|_| Error::from_reason(format!("{label} exceeds the u64 bridge")))?;
    js_number_from_u64(value, label)
}

pub(super) fn doc_id_from_input(doc_id: i64) -> Result<u64> {
    if doc_id < 0 {
        return Err(Error::from_reason("docId must be non-negative"));
    }
    if doc_id > MAX_SAFE_INTEGER {
        return Err(Error::from_reason(
            "docId exceeds JavaScript's safe integer range",
        ));
    }
    u64::try_from(doc_id).map_err(|_| Error::from_reason("docId must be non-negative"))
}

pub(super) fn batch_from_input(
    statements: Vec<(String, Vec<ParamInput<'_>>)>,
) -> Result<Vec<(String, Vec<CoreSQLParam>)>> {
    statements
        .into_iter()
        .map(|(sql, params)| Ok((sql, params_from_input(Some(params))?)))
        .collect()
}

pub(super) fn compression_options(
    options: Option<CompressionOptions>,
) -> Result<SQLiteCompressionOptions> {
    let options = options.unwrap_or(CompressionOptions {
        codec: None,
        page_size: None,
        chunk_pages: None,
        level: None,
    });
    let codec = options.codec.as_deref().unwrap_or("zstd");
    let mut resolved = match codec.to_ascii_lowercase().as_str() {
        "zstd" => SQLiteCompressionOptions::zstd(),
        "lz4" => SQLiteCompressionOptions::lz4(),
        other => {
            return Err(Error::from_reason(format!(
                "unsupported compression codec `{other}`"
            )));
        }
    };
    if let Some(value) = options.page_size {
        resolved.page_size = value;
    }
    if let Some(value) = options.chunk_pages {
        resolved.chunk_pages = value;
    }
    if let Some(value) = options.level {
        resolved.level = value;
    }
    resolved.validate().map_err(Error::from_reason)
}

pub(super) fn scoring_mode(
    engine: &CoreEngine,
    table: &str,
    field: &str,
    scoring: &str,
) -> Result<ScoringMode> {
    match scoring.to_ascii_lowercase().as_str() {
        "bm25" => Ok(ScoringMode::BM25(BM25Params::default())),
        "bayesian" | "bayesian_bm25" => Ok(ScoringMode::BayesianBM25(
            engine
                .bayesian_params_for(table, field)
                .map_err(runtime_error)?,
        )),
        other => Err(Error::from_reason(format!(
            "unsupported scoring mode `{other}`"
        ))),
    }
}

pub(super) fn database_file_format_name(format: DatabaseFileFormat) -> &'static str {
    match format {
        DatabaseFileFormat::Missing => "missing",
        DatabaseFileFormat::PlainSQLite => "sqlite",
        DatabaseFileFormat::CompressedContainer { encrypted: false } => "compressed",
        DatabaseFileFormat::CompressedContainer { encrypted: true } => "compressed_encrypted",
        DatabaseFileFormat::Unrecognized => "unrecognized",
    }
}

pub(super) fn parse_scoring_params(name: &str, json: &str) -> Result<BTreeMap<String, f64>> {
    serde_json::from_str(json).map_err(|err| {
        Error::from_reason(format!(
            "scoring params `{name}` are not a map of floats: {err}"
        ))
    })
}

pub(super) fn runtime_error(error: impl std::fmt::Display) -> Error {
    Error::from_reason(error.to_string())
}
