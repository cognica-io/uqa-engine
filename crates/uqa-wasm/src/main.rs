//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Browser (emscripten) bindings for the UQA engine.
//!
//! The whole surface is one C entry point, `uqa_call(handle, request)`,
//! taking and returning JSON: engines live in a handle registry, and
//! the TypeScript wrapper in `js/` turns the dispatch protocol into a
//! typed async API. Encryption (`SQLCipher` and encrypted compressed
//! containers) is deliberately absent from browser builds; plain and
//! compressed persistent databases work on the emscripten filesystem,
//! which the wrapper mounts onto `IndexedDB` (IDBFS) for durability.

// This crate is an FFI boundary: the exported entry points are
// `extern "C"` over raw C strings, so the workspace-wide `unsafe_code`
// deny is relaxed here.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{json, Map as JSONMap, Value as JSON};

use uqa_core::{DecimalValue, Value};
use uqa_engine::{Engine, HybridSearchParams, SQLParam, SQLResult, ScoredEntry, ScoringMode};
use uqa_scoring::{BM25Params, CalibrationReport};
use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions};

static ENGINES: Mutex<BTreeMap<i32, Arc<Engine>>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicI32 = AtomicI32::new(1);

fn main() {}

/// Single dispatch entry point. `handle` is `0` for static methods
/// (`new`, `open`, ...); the returned string is JSON of the form
/// `{"ok": ...}` or `{"error": "..."}` and must be released with
/// [`uqa_free`].
#[no_mangle]
pub extern "C" fn uqa_call(handle: i32, request: *const c_char) -> *mut c_char {
    let response = match parse_request(request) {
        Ok((method, args)) => match std::panic::catch_unwind(|| dispatch(handle, &method, &args)) {
            Ok(Ok(ok)) => json!({ "ok": ok }),
            Ok(Err(message)) => json!({ "error": message }),
            Err(_) => json!({ "error": "engine call panicked" }),
        },
        Err(message) => json!({ "error": message }),
    };
    let text = response.to_string();
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{\"error\":\"interior NUL in response\"}").unwrap())
        .into_raw()
}

/// Release a string returned by [`uqa_call`].
// The pointer is only ever one this crate handed out via
// `CString::into_raw`; the JS wrapper is the sole caller.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn uqa_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(unsafe { CString::from_raw(ptr) });
    }
}

fn parse_request(request: *const c_char) -> Result<(String, JSON), String> {
    if request.is_null() {
        return Err("null request".to_string());
    }
    let text = unsafe { CStr::from_ptr(request) }
        .to_str()
        .map_err(|_| "request is not valid UTF-8".to_string())?;
    let parsed: JSON =
        serde_json::from_str(text).map_err(|err| format!("invalid request JSON: {err}"))?;
    let method = parsed
        .get("method")
        .and_then(JSON::as_str)
        .ok_or_else(|| "request needs a string `method`".to_string())?
        .to_string();
    let args = parsed.get("args").cloned().unwrap_or(JSON::Null);
    Ok((method, args))
}

fn dispatch(handle: i32, method: &str, args: &JSON) -> Result<JSON, String> {
    if handle == 0 {
        return dispatch_static(method, args);
    }
    let engine = ENGINES
        .lock()
        .get(&handle)
        .cloned()
        .ok_or_else(|| format!("unknown engine handle {handle}"))?;
    if method == "close" {
        ENGINES.lock().remove(&handle);
        engine.close();
        return Ok(JSON::Null);
    }
    dispatch_engine(&engine, method, args)
}

fn register(engine: Engine) -> JSON {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    ENGINES.lock().insert(handle, Arc::new(engine));
    json!(handle)
}

fn dispatch_static(method: &str, args: &JSON) -> Result<JSON, String> {
    match method {
        "new" => Ok(register(Engine::new())),
        "open" => {
            let path = req_str(args, "path")?;
            Ok(register(
                Engine::open(Path::new(&path)).map_err(|err| err.to_string())?,
            ))
        }
        "openAuto" => {
            if args.get("key").is_some_and(|key| !key.is_null()) {
                return Err(ENCRYPTION_UNAVAILABLE.to_string());
            }
            let path = req_str(args, "path")?;
            Ok(register(
                Engine::open_auto(Path::new(&path), None).map_err(|err| err.to_string())?,
            ))
        }
        "openCompressed" => {
            let path = req_str(args, "path")?;
            Ok(register(
                Engine::open_compressed(Path::new(&path), compression_options(args)?)
                    .map_err(|err| err.to_string())?,
            ))
        }
        "openEncrypted" | "openCompressedEncrypted" => Err(ENCRYPTION_UNAVAILABLE.to_string()),
        "detectDatabaseFile" => {
            let path = req_str(args, "path")?;
            let format =
                Engine::detect_database_file(Path::new(&path)).map_err(|err| err.to_string())?;
            Ok(json!(database_file_format_name(format)))
        }
        other => Err(format!("unknown static method `{other}`")),
    }
}

#[allow(clippy::too_many_lines)]
fn dispatch_engine(engine: &Engine, method: &str, args: &JSON) -> Result<JSON, String> {
    match method {
        "sql" => {
            let query = req_str(args, "query")?;
            let params = params_from_json(args.get("params"))?;
            let result = engine.sql(&query, &params).map_err(|err| err.to_string())?;
            Ok(sql_result_to_json(result))
        }
        "sqlBatch" => {
            let statements = args
                .get("statements")
                .and_then(JSON::as_array)
                .ok_or("sqlBatch needs `statements`")?;
            let mut owned: Vec<(String, Vec<SQLParam>)> = Vec::with_capacity(statements.len());
            for statement in statements {
                let pair = statement
                    .as_array()
                    .filter(|pair| pair.len() == 2)
                    .ok_or("each statement must be a [sql, params] pair")?;
                let sql = pair[0]
                    .as_str()
                    .ok_or("statement sql must be a string")?
                    .to_string();
                owned.push((sql, params_from_json(Some(&pair[1]))?));
            }
            let borrowed: Vec<(&str, &[SQLParam])> = owned
                .iter()
                .map(|(sql, params)| (sql.as_str(), params.as_slice()))
                .collect();
            let results = engine.sql_batch(&borrowed).map_err(|err| err.to_string())?;
            Ok(JSON::Array(
                results.into_iter().map(sql_result_to_json).collect(),
            ))
        }
        "createDefaultTable" => {
            engine.create_default_table(&req_str(args, "name")?, req_str_list(args, "ftsFields")?);
            Ok(JSON::Null)
        }
        "createVectorField" => Ok(json!(engine.create_vector_field(
            &req_str(args, "table")?,
            &req_str(args, "field")?,
            req_u64(args, "dimensions")? as u32,
        ))),
        "addDocument" => {
            engine
                .add_document(
                    &req_str(args, "table")?,
                    req_u64(args, "docId")?,
                    document_from_json(args.get("document"))?,
                )
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "addDocumentWithVectors" => {
            engine
                .add_document_with_vector_values(
                    &req_str(args, "table")?,
                    req_u64(args, "docId")?,
                    document_from_json(args.get("document"))?,
                    vector_values_from_json(args.get("vectors"))?,
                )
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "addVector" => Ok(json!(engine.add_vector(
            &req_str(args, "table")?,
            req_u64(args, "docId")?,
            &req_str(args, "field")?,
            req_f32_list(args, "vector")?,
        ))),
        "addVectorValues" => Ok(json!(engine.add_vector_values(
            &req_str(args, "table")?,
            req_u64(args, "docId")?,
            &req_str(args, "field")?,
            req_f32_rows(args, "vectors")?,
        ))),
        "getDocument" => Ok(engine
            .get_document(&req_str(args, "table")?, req_u64(args, "docId")?)
            .map_or(JSON::Null, |document| {
                JSON::Object(
                    document
                        .into_iter()
                        .map(|(key, value)| (key, value_to_json(value)))
                        .collect(),
                )
            })),
        "deleteDocument" => {
            engine
                .delete_document(&req_str(args, "table")?, req_u64(args, "docId")?)
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "documentCount" => Ok(json!(engine.document_count(&req_str(args, "table")?))),
        "search" => {
            let table = req_str(args, "table")?;
            let field = req_str(args, "field")?;
            let scoring = opt_str(args, "scoring").unwrap_or_else(|| "bm25".to_string());
            let mode = scoring_mode(engine, &table, &field, &scoring)?;
            Ok(hits_to_json(engine.search(
                &table,
                &field,
                &req_str(args, "query")?,
                &mode,
                opt_u64(args, "topK")?.unwrap_or(10) as usize,
            )))
        }
        "knnSearch" => Ok(hits_to_json(engine.knn_search(
            &req_str(args, "table")?,
            &req_str(args, "field")?,
            req_f32_list(args, "vector")?,
            opt_u64(args, "topK")?.unwrap_or(10) as usize,
        ))),
        "vectorSimilaritySearch" => Ok(hits_to_json(engine.vector_similarity_search(
            &req_str(args, "table")?,
            &req_str(args, "field")?,
            req_f32_list(args, "vector")?,
            req_f64(args, "threshold")? as f32,
        ))),
        "hybridSearch" => {
            let top_k = opt_u64(args, "topK")?.unwrap_or(10) as usize;
            let params = HybridSearchParams {
                table: &req_str(args, "table")?,
                text_field: &req_str(args, "textField")?,
                text_query: &req_str(args, "textQuery")?,
                vector_field: &req_str(args, "vectorField")?,
                query_vector: req_f32_list(args, "queryVector")?,
                knn_pool: opt_u64(args, "knnPool")?
                    .map_or_else(|| top_k.saturating_mul(4).max(top_k), |pool| pool as usize),
                alpha: opt_f64(args, "alpha")?.unwrap_or(1.0),
                top_k,
            };
            Ok(hits_to_json(engine.hybrid_search(&params)))
        }
        "estimateScoringParams" => {
            let params = engine
                .estimate_scoring_params(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    opt_u64(args, "nSamples")?.unwrap_or(50) as usize,
                    opt_u64(args, "tokensPerQuery")?.unwrap_or(5) as usize,
                    opt_i64(args, "seed")?.unwrap_or(42),
                )
                .map_err(|err| err.to_string())?;
            Ok(float_map_to_json(&params))
        }
        "learnScoringParams" => {
            let params = engine
                .learn_scoring_params(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    &req_str(args, "query")?,
                    &req_labels(args)?,
                )
                .map_err(|err| err.to_string())?;
            Ok(float_map_to_json(&params))
        }
        "updateScoringParams" => {
            let label = req_u64(args, "label")?;
            if label > 1 {
                return Err("label must be 0 or 1".to_string());
            }
            engine
                .update_scoring_params(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    req_f64(args, "score")?,
                    label as u8,
                )
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "calibrationReport" => {
            let report = engine
                .calibration_report(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    &req_str(args, "query")?,
                    &req_labels(args)?,
                )
                .map_err(|err| err.to_string())?;
            Ok(calibration_report_to_json(&report))
        }
        "saveScoringParams" => {
            let params = args
                .get("params")
                .and_then(JSON::as_object)
                .ok_or("saveScoringParams needs a `params` object")?;
            let mut map = BTreeMap::new();
            for (key, value) in params {
                map.insert(
                    key.clone(),
                    value.as_f64().ok_or("scoring params must be numbers")?,
                );
            }
            let json =
                serde_json::to_string(&map).map_err(|err| format!("serialize params: {err}"))?;
            engine
                .save_scoring_params(&req_str(args, "name")?, &json)
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "loadScoringParams" => match engine.load_scoring_params(&req_str(args, "name")?) {
            Some(json) => parse_scoring_params(&json),
            None => Ok(JSON::Null),
        },
        "loadAllScoringParams" => {
            let mut out = JSONMap::new();
            for (name, json) in engine.load_all_scoring_params() {
                out.insert(name, parse_scoring_params(&json)?);
            }
            Ok(JSON::Object(out))
        }
        "dropScoringParams" => Ok(json!(engine.drop_scoring_params(&req_str(args, "name")?))),
        "runCypher" => {
            let params = document_from_json(args.get("params").filter(|p| !p.is_null()))?;
            let (columns, rows) = engine
                .run_cypher(&req_str(args, "graph")?, &req_str(args, "query")?, params)
                .map_err(|err| err.to_string())?;
            Ok(rows_result_to_json(columns, rows, 0))
        }
        "createGraph" => {
            engine.create_graph(&req_str(args, "name")?);
            Ok(JSON::Null)
        }
        "dropGraph" => {
            engine.drop_graph(&req_str(args, "name")?);
            Ok(JSON::Null)
        }
        "listGraphs" => Ok(json!(engine.list_graphs())),
        "listPathIndexes" => Ok(json!(engine.list_path_indexes())),
        "tableNames" => Ok(json!(engine.table_names())),
        "listViews" => Ok(json!(engine.list_views())),
        "listSchemas" => Ok(json!(engine.list_schemas())),
        "listSequences" => Ok(json!(engine.list_sequences())),
        "listNamedAnalyzers" => Ok(json!(engine.list_named_analyzers())),
        "listForeignServers" => Ok(json!(engine.list_foreign_servers())),
        "listForeignTables" => Ok(json!(engine.list_foreign_tables())),
        "takeSQLNotices" => Ok(JSON::Array(
            engine
                .take_sql_notices()
                .into_iter()
                .map(|(level, message)| json!({ "level": level, "message": message }))
                .collect(),
        )),
        "sqlFunctionDepthLimit" => Ok(json!(engine.sql_function_depth_limit())),
        "setSQLFunctionDepthLimit" => {
            engine.set_sql_function_depth_limit(req_u64(args, "limit")? as usize);
            Ok(JSON::Null)
        }
        "cancel" => {
            engine.cancel();
            Ok(JSON::Null)
        }
        other => Err(format!("unknown method `{other}`")),
    }
}

const ENCRYPTION_UNAVAILABLE: &str = "encryption is not available in browser builds";

// ---------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------

fn req_str(args: &JSON, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(JSON::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn opt_str(args: &JSON, key: &str) -> Option<String> {
    args.get(key).and_then(JSON::as_str).map(str::to_string)
}

fn req_str_list(args: &JSON, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(JSON::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("`{key}` must contain strings"))
                })
                .collect()
        })
        .ok_or_else(|| format!("missing list argument `{key}`"))?
}

fn req_u64(args: &JSON, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(JSON::as_u64)
        .ok_or_else(|| format!("missing non-negative integer argument `{key}`"))
}

fn opt_u64(args: &JSON, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a non-negative integer")),
    }
}

fn opt_i64(args: &JSON, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn req_f64(args: &JSON, key: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(JSON::as_f64)
        .ok_or_else(|| format!("missing number argument `{key}`"))
}

fn opt_f64(args: &JSON, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a number")),
    }
}

fn req_f32_list(args: &JSON, key: &str) -> Result<Vec<f32>, String> {
    f32_list(
        args.get(key)
            .ok_or_else(|| format!("missing vector argument `{key}`"))?,
        key,
    )
}

fn req_f32_rows(args: &JSON, key: &str) -> Result<Vec<Vec<f32>>, String> {
    args.get(key)
        .and_then(JSON::as_array)
        .ok_or_else(|| format!("missing tensor argument `{key}`"))?
        .iter()
        .map(|row| f32_list(row, key))
        .collect()
}

fn f32_list(value: &JSON, key: &str) -> Result<Vec<f32>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("`{key}` must be an array of numbers"))?
        .iter()
        .map(|item| {
            item.as_f64()
                .map(|number| number as f32)
                .ok_or_else(|| format!("`{key}` must contain numbers"))
        })
        .collect()
}

fn req_labels(args: &JSON) -> Result<Vec<u8>, String> {
    args.get("labels")
        .and_then(JSON::as_array)
        .ok_or("missing `labels` array")?
        .iter()
        .map(|label| match label.as_u64() {
            Some(value @ (0 | 1)) => Ok(value as u8),
            _ => Err("labels must contain only 0 or 1".to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------
// Value / result conversion
// ---------------------------------------------------------------------

fn value_to_json(value: Value) -> JSON {
    match value {
        Value::Null => JSON::Null,
        Value::Bool(value) => json!(value),
        Value::Int(value) => json!(value),
        Value::Float(value) => serde_json::Number::from_f64(value).map_or(JSON::Null, JSON::Number),
        Value::Decimal(value) => json!(value.to_sql_string()),
        Value::Str(value) => json!(value),
        Value::Bytes(value) => json!({ "$bytes": BASE64.encode(value) }),
        Value::Temporal(value) => json!(value.to_sql_string()),
        Value::List(values) => JSON::Array(values.into_iter().map(value_to_json).collect()),
        Value::Map(values) => JSON::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, value_to_json(value)))
                .collect(),
        ),
    }
}

fn value_from_json(value: &JSON) -> Result<Value, String> {
    match value {
        JSON::Null => Ok(Value::Null),
        JSON::Bool(value) => Ok(Value::Bool(*value)),
        JSON::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = number.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(format!("unsupported number {number}"))
            }
        }
        JSON::String(value) => Ok(Value::Str(value.clone())),
        JSON::Array(values) => Ok(Value::List(
            values
                .iter()
                .map(value_from_json)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        JSON::Object(map) => {
            if map.len() == 1 {
                if let Some(encoded) = map.get("$bytes").and_then(JSON::as_str) {
                    return BASE64
                        .decode(encoded)
                        .map(Value::Bytes)
                        .map_err(|err| format!("invalid $bytes payload: {err}"));
                }
                if let Some(text) = map.get("$decimal").and_then(JSON::as_str) {
                    return DecimalValue::parse(text)
                        .map(Value::Decimal)
                        .ok_or_else(|| format!("invalid $decimal value {text}"));
                }
            }
            let mut out = BTreeMap::new();
            for (key, value) in map {
                out.insert(key.clone(), value_from_json(value)?);
            }
            Ok(Value::Map(out))
        }
    }
}

fn document_from_json(document: Option<&JSON>) -> Result<BTreeMap<String, Value>, String> {
    let Some(document) = document else {
        return Ok(BTreeMap::new());
    };
    let map = document.as_object().ok_or("expected a JSON object")?;
    let mut out = BTreeMap::new();
    for (key, value) in map {
        out.insert(key.clone(), value_from_json(value)?);
    }
    Ok(out)
}

fn vector_values_from_json(
    vectors: Option<&JSON>,
) -> Result<BTreeMap<String, Vec<Vec<f32>>>, String> {
    let Some(vectors) = vectors else {
        return Ok(BTreeMap::new());
    };
    let map = vectors.as_object().ok_or("expected a vectors object")?;
    let mut out = BTreeMap::new();
    for (field, value) in map {
        let rows = if value
            .as_array()
            .is_some_and(|items| items.first().is_some_and(JSON::is_array))
        {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|row| f32_list(row, field))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            vec![f32_list(value, field)?]
        };
        out.insert(field.clone(), rows);
    }
    Ok(out)
}

fn params_from_json(params: Option<&JSON>) -> Result<Vec<SQLParam>, String> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    if params.is_null() {
        return Ok(Vec::new());
    }
    params
        .as_array()
        .ok_or("params must be an array")?
        .iter()
        .map(param_from_json)
        .collect()
}

fn param_from_json(param: &JSON) -> Result<SQLParam, String> {
    if let Some(map) = param.as_object() {
        if map.len() == 1 {
            if let Some(vector) = map.get("$vector") {
                return Ok(SQLParam::vector(f32_list(vector, "$vector")?));
            }
            if let Some(tensor) = map.get("$tensor").and_then(JSON::as_array) {
                let rows = tensor
                    .iter()
                    .map(|row| f32_list(row, "$tensor"))
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(SQLParam::tensor(rows));
            }
        }
    }
    Ok(SQLParam::scalar(value_from_json(param)?))
}

fn sql_result_to_json(result: SQLResult) -> JSON {
    rows_result_to_json(result.columns, result.rows, result.affected_rows)
}

fn rows_result_to_json(
    columns: Vec<String>,
    rows: Vec<BTreeMap<String, Value>>,
    affected_rows: u64,
) -> JSON {
    json!({
        "columns": JSON::from(columns),
        "rows": rows
            .into_iter()
            .map(|row| {
                JSON::Object(
                    row.into_iter()
                        .map(|(key, value)| (key, value_to_json(value)))
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
        "affectedRows": affected_rows,
    })
}

fn hits_to_json(entries: Vec<ScoredEntry>) -> JSON {
    JSON::Array(
        entries
            .into_iter()
            .map(|entry| json!({ "docId": entry.doc_id, "score": entry.score }))
            .collect(),
    )
}

fn float_map_to_json(params: &BTreeMap<String, f64>) -> JSON {
    JSON::Object(
        params
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    serde_json::Number::from_f64(*value).map_or(JSON::Null, JSON::Number),
                )
            })
            .collect(),
    )
}

fn calibration_report_to_json(report: &CalibrationReport) -> JSON {
    json!({
        "ece": report.ece,
        "brier": report.brier,
        "logLoss": report.log_loss,
        "bins": report
            .bins
            .iter()
            .map(|bin| {
                json!({
                    "avgPredicted": bin.avg_predicted,
                    "avgActual": bin.avg_actual,
                    "count": bin.count,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn parse_scoring_params(json: &str) -> Result<JSON, String> {
    let map: BTreeMap<String, f64> = serde_json::from_str(json)
        .map_err(|err| format!("scoring params are not a map of floats: {err}"))?;
    Ok(float_map_to_json(&map))
}

fn compression_options(args: &JSON) -> Result<SQLiteCompressionOptions, String> {
    let codec = opt_str(args, "codec").unwrap_or_else(|| "zstd".to_string());
    let mut options = match codec.to_ascii_lowercase().as_str() {
        "zstd" => SQLiteCompressionOptions::zstd(),
        "lz4" => SQLiteCompressionOptions::lz4(),
        other => return Err(format!("unsupported compression codec `{other}`")),
    };
    if let Some(value) = opt_u64(args, "pageSize")? {
        options.page_size = value as u32;
    }
    if let Some(value) = opt_u64(args, "chunkPages")? {
        options.chunk_pages = value as u32;
    }
    if let Some(value) = opt_i64(args, "level")? {
        options.level = value as i32;
    }
    options.validate()
}

fn scoring_mode(
    engine: &Engine,
    table: &str,
    field: &str,
    scoring: &str,
) -> Result<ScoringMode, String> {
    match scoring.to_ascii_lowercase().as_str() {
        "bm25" => Ok(ScoringMode::BM25(BM25Params::default())),
        "bayesian" | "bayesian_bm25" => Ok(ScoringMode::BayesianBM25(
            engine.bayesian_params_for(table, field),
        )),
        other => Err(format!("unsupported scoring mode `{other}`")),
    }
}

fn database_file_format_name(format: DatabaseFileFormat) -> &'static str {
    match format {
        DatabaseFileFormat::Missing => "missing",
        DatabaseFileFormat::PlainSQLite => "sqlite",
        DatabaseFileFormat::CompressedContainer { encrypted: false } => "compressed",
        DatabaseFileFormat::CompressedContainer { encrypted: true } => "compressed_encrypted",
        DatabaseFileFormat::Unrecognized => "unrecognized",
    }
}
