//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Functional SQL observations. Dispatch happens before Criterion initialization.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::{env, fs};

use serde_json::{json, Value as Json};
use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_scoring::VectorProbabilityTransform;
use uqa_sql::SQLParam;

#[path = "diskann_correctness/dataset.rs"]
mod dataset;
use dataset::{count, digest, Dataset};

const MANIFEST: &[u8] = include_bytes!("../../../../benchmarks/vector-search/manifest.json");
const OUTPUT_ENV: &str = "UQA_DISKANN_CORRECTNESS_OBSERVATIONS";

fn query(k: usize, calibrated: bool) -> String {
    let function = if calibrated {
        "calibrated_vector_match"
    } else {
        "knn_match"
    };
    format!("SELECT id, _score FROM diskann_acceptance WHERE {function}(embedding, $1, {k}) ORDER BY _score DESC, id")
}

fn ranked(engine: &Engine, query: &[f32], k: usize, calibrated: bool) -> Vec<(i64, f64)> {
    engine
        .sql(
            &self::query(k, calibrated),
            &[SQLParam::vector(query.to_vec())],
        )
        .expect("correctness SQL query")
        .rows
        .into_iter()
        .map(|row| {
            let Value::Int(id) = row["id"] else {
                panic!("integer identity required")
            };
            let Value::Float(score) = row["_score"] else {
                panic!("floating score required")
            };
            (id, score)
        })
        .collect()
}

fn observe(
    engine: &Engine,
    data: &Dataset,
    k: usize,
    transform: Option<&VectorProbabilityTransform>,
) -> Json {
    let results: Vec<_> = data
        .queries
        .iter()
        .enumerate()
        .map(|(query_id, query)| {
            let raw = ranked(engine, query, k, false);
            assert_eq!(raw.len(), k);
            let pool: BTreeMap<_, _> = if transform.is_some() {
                ranked(engine, query, k, true).into_iter().collect()
            } else {
                BTreeMap::new()
            };
            if transform.is_some() {
                assert_eq!(pool.len(), raw.len());
            }
            let hits: Vec<_> = raw
                .into_iter()
                .map(|(doc_id, score)| {
                    let mut hit = json!({"doc_id": doc_id, "score": score});
                    if let Some(transform) = transform {
                        hit["pool_probability"] = json!(pool[&doc_id]);
                        hit["fixed_probability"] =
                            json!(transform.calibrate_one(1.0 - score).unwrap());
                    }
                    hit
                })
                .collect();
            json!({"query_id": query_id, "hits": hits})
        })
        .collect();
    json!(results)
}

fn route(engine: &Engine, query: &[f32], k: usize) -> Json {
    let result = engine
        .sql(
            &format!("EXPLAIN (ANALYZE, FORMAT JSON) {}", self::query(k, false)),
            &[SQLParam::vector(query.to_vec())],
        )
        .unwrap();
    let Value::Str(plan) = &result.rows[0]["plan"] else {
        panic!("JSON plan required")
    };
    let plan: Json = serde_json::from_str(plan).unwrap();
    let searches = plan["Vector Searches"].as_array().unwrap();
    assert_eq!(searches.len(), 1);
    let search = &searches[0];
    assert_eq!(search["Route"], "approximate");
    assert_eq!(search["Requested K"], k);
    assert_eq!(search["Returned Documents"], k);
    assert_eq!(search["Scoring"]["Exact"]["Vectors"], 0);
    assert!(search["Traversal"]["PQ Estimates"].as_u64().unwrap() > 0);
    // Keep logical counters; EXPLAIN's elapsed-time fields are not observations here.
    json!({"route": "approximate", "requested_k": k, "returned_documents": k,
        "generation": search["Generation"], "pq_estimates": search["Traversal"]["PQ Estimates"],
        "exact_vectors": search["Scoring"]["Exact"]["Vectors"]})
}

fn populate(engine: &Engine, data: &Dataset, spec: &Json) {
    let shape = if data.tensor { "TENSOR" } else { "VECTOR" };
    engine
        .sql(
            &format!(
                "CREATE TABLE diskann_acceptance(id INTEGER PRIMARY KEY, embedding {shape}({}))",
                count(&spec["dimensions"])
            ),
            &[],
        )
        .unwrap();
    engine
        .transaction(|engine| {
            for (index, row) in data.rows.iter().enumerate() {
                engine.sql(
                    "INSERT INTO diskann_acceptance VALUES($1, $2)",
                    &[SQLParam::scalar(Value::Int(index as i64 + 1)), row.clone()],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn exercise(suite: &Json, spec: &Json, transform: &VectorProbabilityTransform) -> Json {
    let data = dataset::load(spec);
    let directory = tempdir().unwrap();
    let path = directory.path().join("diskann.sqlite3");
    let engine = Engine::open(&path).unwrap();
    populate(&engine, &data, spec);
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    let exact: Vec<_> = spec["candidate_ks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| json!({"candidate_k": k, "results": observe(&engine, &data, count(k), None)}))
        .collect();
    drop(engine);
    let mut cases = Vec::new();
    for seed in suite["seeds"].as_array().unwrap() {
        for search_list in suite["search_list_sizes"].as_array().unwrap() {
            eprintln!(
                "DiskANN correctness: {} seed={seed} search_list={search_list}",
                spec["name"]
            );
            let settings = &suite["index_parameters"];
            let engine = Engine::open(&path).unwrap();
            engine.sql(&format!("CREATE INDEX diskann_acceptance_idx ON diskann_acceptance USING diskann(embedding) WITH(max_degree={}, build_list_size={}, alpha={}, beam_width={}, pq_bytes={}, seed={seed}, search_list_size={search_list})", settings["max_degree"], settings["build_list_size"], settings["alpha"], settings["beam_width"], spec["pq_bytes"]), &[]).unwrap();
            drop(engine);
            // All owning handles close before reopen; this does not evict the OS file cache.
            let engine = Engine::open(&path).unwrap();
            for k in spec["candidate_ks"].as_array().unwrap() {
                let results = observe(&engine, &data, count(k), Some(transform));
                let diagnostic = route(&engine, &data.queries[0], count(k));
                cases.push(
                    json!({"seed": seed, "search_list_size": search_list, "candidate_k": k,
                    "diagnostic": diagnostic, "results": results}),
                );
            }
            engine
                .sql("DROP INDEX diskann_acceptance_idx", &[])
                .unwrap();
            drop(engine);
        }
    }
    json!({"name": spec["name"], "exact": exact, "cases": cases})
}

pub(super) fn run() {
    let manifest: Json = serde_json::from_slice(MANIFEST).unwrap();
    let suite = &manifest["correctness"];
    let parameters = &suite["fixed_transform"];
    let transform = VectorProbabilityTransform::new(
        parameters["mu_match"].as_f64().unwrap(),
        parameters["mu_random"].as_f64().unwrap(),
        parameters["sigma"].as_f64().unwrap(),
        parameters["base_rate"].as_f64().unwrap(),
    )
    .unwrap();
    let fixtures: Vec<_> = suite["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| exercise(suite, spec, &transform))
        .collect();
    let mut executable = fs::File::open(env::current_exe().unwrap()).unwrap();
    let mut hash = sha2::Sha256::default();
    let mut buffer = [0_u8; 8192];
    loop {
        let size = executable.read(&mut buffer).unwrap();
        if size == 0 {
            break;
        }
        sha2::Digest::update(&mut hash, &buffer[..size]);
    }
    let executable_sha256 = format!("{:x}", sha2::Digest::finalize(hash));
    let observations = json!({"schema_version": 3, "mode": "correctness", "suite": suite,
        "manifest_sha256": digest(MANIFEST), "executable_sha256": executable_sha256, "fixtures": fixtures});
    let output =
        PathBuf::from(env::var_os(OUTPUT_ENV).unwrap_or_else(|| {
            "target/benchmark-runs/diskann-correctness-observations.json".into()
        }));
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, serde_json::to_vec(&observations).unwrap()).unwrap();
    eprintln!("DiskANN correctness observations: {}", output.display());
}
