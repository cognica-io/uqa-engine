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
const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

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
    response_c_string(text)
}

fn response_c_string(text: String) -> *mut c_char {
    match CString::new(text) {
        Ok(response) => response.into_raw(),
        Err(_) => match CString::new("{\"error\":\"interior NUL in response\"}") {
            Ok(response) => response.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
    }
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
        engine.close().map_err(|err| err.to_string())?;
        ENGINES
            .lock()
            .remove(&handle)
            .ok_or_else(|| format!("engine handle {handle} was closed concurrently"))?;
        return Ok(JSON::Null);
    }
    if method == "newSession" {
        return register(engine.new_session().map_err(|err| err.to_string())?);
    }
    dispatch_engine(&engine, method, args)
}

fn register(engine: Engine) -> Result<JSON, String> {
    let handle = NEXT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1).filter(|next| *next > 0)
        })
        .map_err(|_| "engine handle space is exhausted".to_string())?;
    let mut engines = ENGINES.lock();
    match engines.entry(handle) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(Arc::new(engine));
        }
        std::collections::btree_map::Entry::Occupied(_) => {
            return Err(format!("engine handle collision for {handle}"));
        }
    }
    Ok(json!(handle))
}

fn dispatch_static(method: &str, args: &JSON) -> Result<JSON, String> {
    match method {
        "new" => register(Engine::new()),
        "open" => {
            let path = req_str(args, "path")?;
            register(Engine::open(Path::new(&path)).map_err(|err| err.to_string())?)
        }
        "openAuto" => {
            if args.get("key").is_some_and(|key| !key.is_null()) {
                return Err(ENCRYPTION_UNAVAILABLE.to_string());
            }
            let path = req_str(args, "path")?;
            register(Engine::open_auto(Path::new(&path), None).map_err(|err| err.to_string())?)
        }
        "openCompressed" => {
            let path = req_str(args, "path")?;
            register(
                Engine::open_compressed(Path::new(&path), compression_options(args)?)
                    .map_err(|err| err.to_string())?,
            )
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
            sql_result_to_json(result)
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
            results
                .into_iter()
                .map(sql_result_to_json)
                .collect::<Result<Vec<_>, _>>()
                .map(JSON::Array)
        }
        "createDefaultTable" => {
            engine
                .create_default_table(&req_str(args, "name")?, req_str_list(args, "ftsFields")?)
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "createVectorField" => Ok(json!(engine
            .create_vector_field(
                &req_str(args, "table")?,
                &req_str(args, "field")?,
                req_u32(args, "dimensions")?,
            )
            .map_err(|err| err.to_string())?)),
        "addDocument" => {
            engine
                .add_document(
                    &req_str(args, "table")?,
                    req_u64(args, "docId")?,
                    document_from_json(
                        args.get("document")
                            .ok_or("addDocument needs a `document` object")?,
                    )?,
                )
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "addDocumentWithVectors" => {
            engine
                .add_document_with_vector_values(
                    &req_str(args, "table")?,
                    req_u64(args, "docId")?,
                    document_from_json(
                        args.get("document")
                            .ok_or("addDocumentWithVectors needs a `document` object")?,
                    )?,
                    vector_values_from_json(
                        args.get("vectors")
                            .ok_or("addDocumentWithVectors needs a `vectors` object")?,
                    )?,
                )
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "addVector" => Ok(json!(engine
            .add_vector(
                &req_str(args, "table")?,
                req_u64(args, "docId")?,
                &req_str(args, "field")?,
                req_f32_list(args, "vector")?,
            )
            .map_err(|err| err.to_string())?)),
        "addVectorValues" => Ok(json!(engine
            .add_vector_values(
                &req_str(args, "table")?,
                req_u64(args, "docId")?,
                &req_str(args, "field")?,
                req_f32_rows(args, "vectors")?,
            )
            .map_err(|err| err.to_string())?)),
        "getDocument" => match engine
            .get_document(&req_str(args, "table")?, req_u64(args, "docId")?)
            .map_err(|err| err.to_string())?
        {
            Some(document) => document
                .into_iter()
                .map(|(key, value)| Ok((key, value_to_json(value)?)))
                .collect::<Result<JSONMap<_, _>, String>>()
                .map(JSON::Object),
            None => Ok(JSON::Null),
        },
        "deleteDocument" => {
            engine
                .delete_document(&req_str(args, "table")?, req_u64(args, "docId")?)
                .map_err(|err| err.to_string())?;
            Ok(JSON::Null)
        }
        "documentCount" => json_u64(
            engine
                .document_count(&req_str(args, "table")?)
                .map_err(|err| err.to_string())?,
            "document count",
        ),
        "search" => {
            let table = req_str(args, "table")?;
            let field = req_str(args, "field")?;
            let scoring = opt_str(args, "scoring")?.unwrap_or_else(|| "bm25".to_string());
            let mode = scoring_mode(engine, &table, &field, &scoring)?;
            hits_to_json(
                engine
                    .search(
                        &table,
                        &field,
                        &req_str(args, "query")?,
                        &mode,
                        opt_usize(args, "topK")?.unwrap_or(10),
                    )
                    .map_err(|err| err.to_string())?,
            )
        }
        "knnSearch" => hits_to_json(
            engine
                .knn_search(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    req_f32_list(args, "vector")?,
                    opt_usize(args, "topK")?.unwrap_or(10),
                )
                .map_err(|err| err.to_string())?,
        ),
        "vectorSimilaritySearch" => hits_to_json(
            engine
                .vector_similarity_search(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    req_f32_list(args, "vector")?,
                    f32_from_f64(req_f64(args, "threshold")?, "threshold")?,
                )
                .map_err(|err| err.to_string())?,
        ),
        "hybridSearch" => {
            let top_k = opt_usize(args, "topK")?.unwrap_or(10);
            let knn_pool = match opt_usize(args, "knnPool")? {
                Some(pool) => pool,
                None => top_k
                    .checked_mul(4)
                    .ok_or("default knnPool exceeds this build's addressable range")?,
            };
            let alpha = opt_f64(args, "alpha")?.unwrap_or(1.0);
            if !alpha.is_finite() {
                return Err("alpha must be finite".to_string());
            }
            let params = HybridSearchParams {
                table: &req_str(args, "table")?,
                text_field: &req_str(args, "textField")?,
                text_query: &req_str(args, "textQuery")?,
                vector_field: &req_str(args, "vectorField")?,
                query_vector: req_f32_list(args, "queryVector")?,
                knn_pool,
                alpha,
                top_k,
            };
            hits_to_json(
                engine
                    .hybrid_search(&params)
                    .map_err(|err| err.to_string())?,
            )
        }
        "estimateScoringParams" => {
            let params = engine
                .estimate_scoring_params(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    opt_usize(args, "nSamples")?.unwrap_or(50),
                    opt_usize(args, "tokensPerQuery")?.unwrap_or(5),
                    opt_i64(args, "seed")?.unwrap_or(42),
                )
                .map_err(|err| err.to_string())?;
            float_map_to_json(&params)
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
            float_map_to_json(&params)
        }
        "updateScoringParams" => {
            let label = binary_label(req_u64(args, "label")?)?;
            engine
                .update_scoring_params(
                    &req_str(args, "table")?,
                    &req_str(args, "field")?,
                    req_f64(args, "score")?,
                    label,
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
            calibration_report_to_json(&report)
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
        "loadScoringParams" => match engine
            .load_scoring_params(&req_str(args, "name")?)
            .map_err(|err| err.to_string())?
        {
            Some(json) => parse_scoring_params(&json),
            None => Ok(JSON::Null),
        },
        "loadAllScoringParams" => {
            let mut out = JSONMap::new();
            for (name, json) in engine
                .load_all_scoring_params()
                .map_err(|err| err.to_string())?
            {
                out.insert(name, parse_scoring_params(&json)?);
            }
            Ok(JSON::Object(out))
        }
        "dropScoringParams" => Ok(json!(engine
            .drop_scoring_params(&req_str(args, "name")?)
            .map_err(|err| err.to_string())?)),
        "runCypher" => {
            let params = args
                .get("params")
                .filter(|params| !params.is_null())
                .map(document_from_json)
                .transpose()?
                .unwrap_or_default();
            let (columns, rows) = engine
                .run_cypher(&req_str(args, "graph")?, &req_str(args, "query")?, params)
                .map_err(|err| err.to_string())?;
            rows_result_to_json(columns, rows, 0)
        }
        "createGraph" => Ok(json!(engine
            .create_graph(&req_str(args, "name")?)
            .map_err(|err| err.to_string())?)),
        "dropGraph" => Ok(json!(engine
            .drop_graph(&req_str(args, "name")?)
            .map_err(|err| err.to_string())?)),
        "listGraphs" => Ok(json!(engine.list_graphs().map_err(|err| err.to_string())?)),
        "listPathIndexes" => Ok(json!(engine
            .list_path_indexes()
            .map_err(|err| err.to_string())?)),
        "tableNames" => Ok(json!(engine.table_names().map_err(|err| err.to_string())?)),
        "listViews" => Ok(json!(engine.list_views().map_err(|err| err.to_string())?)),
        "listSchemas" => Ok(json!(engine
            .list_schemas()
            .map_err(|err| err.to_string())?)),
        "listSequences" => Ok(json!(engine
            .list_sequences()
            .map_err(|err| err.to_string())?)),
        "listNamedAnalyzers" => Ok(json!(engine
            .list_named_analyzers()
            .map_err(|err| err.clone())?)),
        "listForeignServers" => Ok(json!(engine
            .list_foreign_servers()
            .map_err(|err| err.clone())?)),
        "listForeignTables" => Ok(json!(engine
            .list_foreign_tables()
            .map_err(|err| err.clone())?)),
        "takeSQLNotices" => Ok(JSON::Array(
            engine
                .take_sql_notices()
                .into_iter()
                .map(|(level, message)| json!({ "level": level, "message": message }))
                .collect(),
        )),
        "sqlFunctionDepthLimit" => json_u64(
            u64::try_from(engine.sql_function_depth_limit())
                .map_err(|_| "SQL function depth limit exceeds the u64 bridge")?,
            "SQL function depth limit",
        ),
        "setSQLFunctionDepthLimit" => {
            engine.set_sql_function_depth_limit(req_usize(args, "limit")?);
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

fn opt_str(args: &JSON, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a string")),
    }
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
    let value = args
        .get(key)
        .and_then(JSON::as_u64)
        .ok_or_else(|| format!("missing non-negative integer argument `{key}`"))?;
    if value > JS_MAX_SAFE_INTEGER {
        return Err(format!(
            "`{key}` exceeds JavaScript's maximum safe integer ({JS_MAX_SAFE_INTEGER})"
        ));
    }
    Ok(value)
}

fn req_u32(args: &JSON, key: &str) -> Result<u32, String> {
    u32::try_from(req_u64(args, key)?)
        .map_err(|_| format!("`{key}` exceeds the maximum 32-bit unsigned integer"))
}

fn req_usize(args: &JSON, key: &str) -> Result<usize, String> {
    usize::try_from(req_u64(args, key)?)
        .map_err(|_| format!("`{key}` exceeds this WebAssembly build's addressable range"))
}

fn opt_u64(args: &JSON, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("`{key}` must be a non-negative integer"))?;
            if value > JS_MAX_SAFE_INTEGER {
                return Err(format!(
                    "`{key}` exceeds JavaScript's maximum safe integer ({JS_MAX_SAFE_INTEGER})"
                ));
            }
            Ok(Some(value))
        }
    }
}

fn opt_usize(args: &JSON, key: &str) -> Result<Option<usize>, String> {
    opt_u64(args, key)?
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| format!("`{key}` exceeds this WebAssembly build's addressable range"))
        })
        .transpose()
}

fn opt_i64(args: &JSON, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_i64()
                .ok_or_else(|| format!("`{key}` must be an integer"))?;
            if value.unsigned_abs() > JS_MAX_SAFE_INTEGER {
                return Err(format!(
                    "`{key}` exceeds JavaScript's safe integer range: {value}"
                ));
            }
            Ok(Some(value))
        }
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
            let number = item
                .as_f64()
                .ok_or_else(|| format!("`{key}` must contain numbers"))?;
            f32_from_f64(number, key)
        })
        .collect()
}

fn f32_from_f64(value: f64, context: &str) -> Result<f32, String> {
    if !value.is_finite() {
        return Err(format!("`{context}` must be finite, got {value}"));
    }
    if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(format!("`{context}` is outside the f32 range: {value}"));
    }
    Ok(value as f32)
}

fn binary_label(value: u64) -> Result<u8, String> {
    match value {
        0 | 1 => u8::try_from(value).map_err(|_| "label exceeds the u8 bridge".to_string()),
        _ => Err("label must be 0 or 1".to_string()),
    }
}

fn req_labels(args: &JSON) -> Result<Vec<u8>, String> {
    args.get("labels")
        .and_then(JSON::as_array)
        .ok_or("missing `labels` array")?
        .iter()
        .map(|label| {
            binary_label(
                label
                    .as_u64()
                    .ok_or_else(|| "labels must contain only 0 or 1".to_string())?,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------
// Value / result conversion
// ---------------------------------------------------------------------

fn value_to_json(value: Value) -> Result<JSON, String> {
    match value {
        Value::Null => Ok(JSON::Null),
        Value::Bool(value) => Ok(json!(value)),
        Value::Int(value) => json_i64(value, "SQL integer"),
        Value::Float(value) => json_number(value, "SQL float"),
        Value::Decimal(value) => Ok(json!(value.to_sql_string())),
        Value::Str(value) => Ok(json!(value)),
        Value::Bytes(value) => Ok(json!({ "$bytes": BASE64.encode(value) })),
        Value::Temporal(value) => Ok(json!(value.to_sql_string())),
        Value::List(values) => values
            .into_iter()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(JSON::Array),
        Value::Map(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, value_to_json(value)?)))
            .collect::<Result<JSONMap<_, _>, String>>()
            .map(JSON::Object),
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
                if let Some(encoded) = map.get("$bytes") {
                    let encoded = encoded
                        .as_str()
                        .ok_or("$bytes payload must be a base64 string")?;
                    return BASE64
                        .decode(encoded)
                        .map(Value::Bytes)
                        .map_err(|err| format!("invalid $bytes payload: {err}"));
                }
                if let Some(text) = map.get("$decimal") {
                    let text = text.as_str().ok_or("$decimal payload must be a string")?;
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

fn document_from_json(document: &JSON) -> Result<BTreeMap<String, Value>, String> {
    let map = document.as_object().ok_or("expected a JSON object")?;
    let mut out = BTreeMap::new();
    for (key, value) in map {
        out.insert(key.clone(), value_from_json(value)?);
    }
    Ok(out)
}

fn vector_values_from_json(vectors: &JSON) -> Result<BTreeMap<String, Vec<Vec<f32>>>, String> {
    let map = vectors.as_object().ok_or("expected a vectors object")?;
    let mut out = BTreeMap::new();
    for (field, value) in map {
        let rows = if let Some(items) = value
            .as_array()
            .filter(|items| items.first().is_some_and(JSON::is_array))
        {
            items
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
            if let Some(tensor) = map.get("$tensor") {
                let tensor = tensor
                    .as_array()
                    .ok_or("$tensor payload must be an array of vectors")?;
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

fn sql_result_to_json(result: SQLResult) -> Result<JSON, String> {
    rows_result_to_json(result.columns, result.rows, result.affected_rows)
}

fn rows_result_to_json(
    columns: Vec<String>,
    rows: Vec<BTreeMap<String, Value>>,
    affected_rows: u64,
) -> Result<JSON, String> {
    let rows = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(key, value)| Ok((key, value_to_json(value)?)))
                .collect::<Result<JSONMap<_, _>, String>>()
                .map(JSON::Object)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "columns": JSON::from(columns),
        "rows": rows,
        "affectedRows": json_u64(affected_rows, "affected row count")?,
    }))
}

fn hits_to_json(entries: Vec<ScoredEntry>) -> Result<JSON, String> {
    entries
        .into_iter()
        .map(|entry| {
            Ok(json!({
                "docId": json_u64(entry.doc_id, "search document id")?,
                "score": json_number(entry.score, "search score")?,
            }))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(JSON::Array)
}

fn float_map_to_json(params: &BTreeMap<String, f64>) -> Result<JSON, String> {
    params
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                json_number(*value, &format!("scoring parameter `{key}`"))?,
            ))
        })
        .collect::<Result<JSONMap<_, _>, String>>()
        .map(JSON::Object)
}

fn calibration_report_to_json(report: &CalibrationReport) -> Result<JSON, String> {
    let bins = report
        .bins
        .iter()
        .map(|bin| {
            Ok(json!({
                "avgPredicted": json_number(bin.avg_predicted, "calibration avgPredicted")?,
                "avgActual": json_number(bin.avg_actual, "calibration avgActual")?,
                "count": json_u64(
                    u64::try_from(bin.count)
                        .map_err(|_| "calibration bin count exceeds the u64 bridge")?,
                    "calibration bin count",
                )?,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({
        "ece": json_number(report.ece, "calibration ece")?,
        "brier": json_number(report.brier, "calibration brier")?,
        "logLoss": json_number(report.log_loss, "calibration logLoss")?,
        "bins": bins,
    }))
}

fn parse_scoring_params(json: &str) -> Result<JSON, String> {
    let map: BTreeMap<String, f64> = serde_json::from_str(json)
        .map_err(|err| format!("scoring params are not a map of floats: {err}"))?;
    float_map_to_json(&map)
}

fn json_number(value: f64, context: &str) -> Result<JSON, String> {
    serde_json::Number::from_f64(value)
        .map(JSON::Number)
        .ok_or_else(|| format!("{context} is not a finite JSON number: {value}"))
}

fn json_u64(value: u64, context: &str) -> Result<JSON, String> {
    if value > JS_MAX_SAFE_INTEGER {
        return Err(format!(
            "{context} exceeds JavaScript's maximum safe integer ({JS_MAX_SAFE_INTEGER}): {value}"
        ));
    }
    Ok(json!(value))
}

fn json_i64(value: i64, context: &str) -> Result<JSON, String> {
    if value.unsigned_abs() > JS_MAX_SAFE_INTEGER {
        return Err(format!(
            "{context} exceeds JavaScript's safe integer range: {value}"
        ));
    }
    Ok(json!(value))
}

fn compression_options(args: &JSON) -> Result<SQLiteCompressionOptions, String> {
    let codec = opt_str(args, "codec")?.unwrap_or_else(|| "zstd".to_string());
    let mut options = match codec.to_ascii_lowercase().as_str() {
        "zstd" => SQLiteCompressionOptions::zstd(),
        "lz4" => SQLiteCompressionOptions::lz4(),
        other => return Err(format!("unsupported compression codec `{other}`")),
    };
    if let Some(value) = opt_u64(args, "pageSize")? {
        options.page_size = u32::try_from(value)
            .map_err(|_| "`pageSize` exceeds the maximum 32-bit unsigned integer".to_string())?;
    }
    if let Some(value) = opt_u64(args, "chunkPages")? {
        options.chunk_pages = u32::try_from(value)
            .map_err(|_| "`chunkPages` exceeds the maximum 32-bit unsigned integer".to_string())?;
    }
    if let Some(value) = opt_i64(args, "level")? {
        options.level = i32::try_from(value)
            .map_err(|_| "`level` exceeds the signed 32-bit integer range".to_string())?;
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
            engine
                .bayesian_params_for(table, field)
                .map_err(|err| err.to_string())?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_bridge_rejects_values_it_cannot_represent_losslessly() {
        assert!(value_to_json(Value::Float(f64::NAN)).is_err());
        assert!(json_u64(JS_MAX_SAFE_INTEGER + 1, "test integer").is_err());
        assert!(json_i64(-(JS_MAX_SAFE_INTEGER as i64) - 1, "test integer").is_err());
    }

    #[test]
    fn numeric_arguments_do_not_truncate() {
        let args = json!({ "dimensions": u64::from(u32::MAX) + 1 });
        assert!(req_u32(&args, "dimensions").is_err());
    }

    #[test]
    fn malformed_optional_and_tagged_values_are_not_defaulted() {
        assert!(opt_str(&json!({ "scoring": 7 }), "scoring").is_err());
        assert!(value_from_json(&json!({ "$bytes": 7 })).is_err());
        assert!(param_from_json(&json!({ "$tensor": "not-a-tensor" })).is_err());

        let engine = Engine::new();
        let args = json!({ "table": "docs", "docId": 1 });
        assert!(dispatch_engine(&engine, "addDocument", &args).is_err());
    }

    #[test]
    fn graph_mutation_status_is_preserved() {
        let engine = Engine::new();
        let args = json!({ "name": "g" });
        assert_eq!(
            dispatch_engine(&engine, "createGraph", &args).unwrap(),
            json!(true)
        );
        assert_eq!(
            dispatch_engine(&engine, "createGraph", &args).unwrap(),
            json!(false)
        );
        assert_eq!(
            dispatch_engine(&engine, "dropGraph", &args).unwrap(),
            json!(true)
        );
        assert_eq!(
            dispatch_engine(&engine, "dropGraph", &args).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn sql_execution_errors_cross_the_dispatch_boundary() {
        let engine = Engine::new();
        let args = json!({ "query": "SELECT * FROM missing_table" });
        let error = dispatch_engine(&engine, "sql", &args).unwrap_err();
        assert!(error.contains("missing_table"), "{error}");
    }
}
