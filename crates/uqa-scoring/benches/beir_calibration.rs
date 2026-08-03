//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! BEIR-style vector calibration and hybrid retrieval benchmarks.
//!
//! The default fixture is deterministic and self-contained so CI can
//! compile and run the benchmark without external data. Set
//! `UQA_BENCH_REAL_BEIR=1` to load real BEIR-prepared JSON and NPY data
//! from the directory specified by `UQA_BENCH_BEIR_DIR`. That directory
//! must contain a child named by `UQA_BENCH_BEIR_DATASET` (default:
//! `nfcorpus`).

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::Value as JSONValue;
use uqa_scoring::{
    average_precision_at_k, log_odds_conjunction, ndcg_at_k, CalibrationMetrics,
    VectorProbabilityTransform, VectorScorer,
};

const SYNTHETIC_DOCS: usize = 240;
const SYNTHETIC_QUERIES: usize = 40;
const SYNTHETIC_DIM: usize = 32;
const SYNTHETIC_K: usize = 50;
const REAL_K: usize = 1_000;
const REAL_QUERY_LIMIT: usize = 50;
const METRIC_K: usize = 10;

#[derive(Clone)]
struct DocumentCase {
    vector: Vec<f32>,
    tokens: Vec<String>,
}

#[derive(Clone)]
struct QueryCase {
    vector: Vec<f32>,
    relevant: BTreeMap<usize, f64>,
    tokens: Vec<String>,
}

struct Fixture {
    name: String,
    corpus: Vec<DocumentCase>,
    queries: Vec<QueryCase>,
    k: usize,
}

#[derive(Clone, Copy)]
enum CalibrationSource {
    DistanceGap,
    BayesianBM25,
    DensityPrior,
}

#[derive(Clone, Copy, Default)]
struct MethodReport {
    ndcg: f64,
    map: f64,
    recall: f64,
    ece: f64,
    brier: f64,
    log_loss: f64,
}

fn vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 32) as u32 as f32) / (u32::MAX as f32)
        })
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn synthetic_fixture() -> Fixture {
    let mut corpus = Vec::with_capacity(SYNTHETIC_DOCS);
    for i in 0..SYNTHETIC_DOCS {
        let topic = i % SYNTHETIC_QUERIES;
        let title = format!("topic_{topic} title_{i}");
        let body = format!(
            "topic_{topic} body cluster_{} shared evidence document_{i}",
            i % 12
        );
        let mut doc_vector = vector(i as u64 + 11, SYNTHETIC_DIM);
        doc_vector[0] = topic as f32 / SYNTHETIC_QUERIES as f32;
        let tokens = tokenize(&format!("{title} {body}"));
        corpus.push(DocumentCase {
            vector: doc_vector,
            tokens,
        });
    }

    let mut queries = Vec::with_capacity(SYNTHETIC_QUERIES);
    for q in 0..SYNTHETIC_QUERIES {
        let anchor = q * 5 % SYNTHETIC_DOCS;
        let mut query_vector = corpus[anchor].vector.clone();
        query_vector[0] = q as f32 / SYNTHETIC_QUERIES as f32;
        let text = format!("topic_{q} evidence");
        let mut relevant = BTreeMap::new();
        for idx in 0..corpus.len() {
            if idx % SYNTHETIC_QUERIES == q {
                relevant.insert(idx, if idx == anchor { 3.0 } else { 1.0 });
            }
        }
        queries.push(QueryCase {
            vector: query_vector,
            relevant,
            tokens: tokenize(&text),
        });
    }

    Fixture {
        name: "synthetic".to_string(),
        corpus,
        queries,
        k: SYNTHETIC_K,
    }
}

fn fixture() -> Fixture {
    match optional_env("UQA_BENCH_REAL_BEIR")
        .unwrap_or_else(|error| panic!("invalid BEIR benchmark environment: {error}"))
        .as_deref()
    {
        Some("1") => load_real_beir_fixture()
            .unwrap_or_else(|error| panic!("failed to load requested real BEIR fixture: {error}")),
        None | Some("0") => synthetic_fixture(),
        Some(value) => panic!("UQA_BENCH_REAL_BEIR must be `0` or `1`, got `{value}`"),
    }
}

fn load_real_beir_fixture() -> Result<Fixture, String> {
    let dataset = optional_env("UQA_BENCH_BEIR_DATASET")?.unwrap_or_else(|| "nfcorpus".to_string());
    let root = optional_env("UQA_BENCH_BEIR_DIR")?
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "UQA_BENCH_BEIR_DIR is required when UQA_BENCH_REAL_BEIR=1".to_string())?;
    load_beir_dataset(&dataset, &root.join(&dataset))
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name}: {error}")),
    }
}

fn load_beir_dataset(name: &str, path: &Path) -> Result<Fixture, String> {
    let corpus_json = read_json(&path.join("corpus.json"))?;
    let queries_json = read_json(&path.join("queries.json"))?;
    let qrels_json = read_json(&path.join("qrels.json"))?;
    let corpus_vectors = read_npy_matrix(&path.join("corpus_embeddings.npy"))?;
    let query_vectors = read_npy_matrix(&path.join("query_embeddings.npy"))?;

    let doc_ids = string_array(&corpus_json, "doc_ids")?;
    let titles = string_array(&corpus_json, "titles")?;
    let texts = string_array(&corpus_json, "texts")?;
    let query_ids = string_array(&queries_json, "query_ids")?;
    let query_texts = string_array(&queries_json, "texts")?;
    let qrel_query_ids = string_array(&qrels_json, "query_ids")?;
    let qrel_doc_ids = string_array(&qrels_json, "doc_ids")?;
    let qrel_scores = number_array(&qrels_json, "scores")?;

    if doc_ids.len() != titles.len()
        || doc_ids.len() != texts.len()
        || doc_ids.len() != corpus_vectors.len()
        || query_ids.len() != query_texts.len()
        || query_ids.len() != query_vectors.len()
        || qrel_query_ids.len() != qrel_doc_ids.len()
        || qrel_query_ids.len() != qrel_scores.len()
    {
        return Err("BEIR fixture arrays have inconsistent lengths".into());
    }

    let doc_index: BTreeMap<&str, usize> = doc_ids
        .iter()
        .enumerate()
        .map(|(idx, id)| (id.as_str(), idx))
        .collect();
    let mut qrels_by_query: BTreeMap<&str, BTreeMap<usize, f64>> = BTreeMap::new();
    for ((qid, did), score) in qrel_query_ids.iter().zip(&qrel_doc_ids).zip(&qrel_scores) {
        if let Some(doc_idx) = doc_index.get(did.as_str()) {
            if *score > 0.0 {
                qrels_by_query
                    .entry(qid.as_str())
                    .or_default()
                    .insert(*doc_idx, *score);
            }
        }
    }

    let corpus: Vec<DocumentCase> = doc_ids
        .into_iter()
        .zip(titles)
        .zip(texts)
        .zip(corpus_vectors)
        .map(|(((_id, title), body), vector)| {
            let tokens = tokenize(&format!("{title} {body}"));
            DocumentCase { vector, tokens }
        })
        .collect();

    let queries: Vec<QueryCase> = query_ids
        .into_iter()
        .zip(query_texts)
        .zip(query_vectors)
        .filter_map(|((id, text), vector)| {
            let relevant = qrels_by_query.get(id.as_str())?.clone();
            Some(QueryCase {
                vector,
                relevant,
                tokens: tokenize(&text),
            })
        })
        .take(REAL_QUERY_LIMIT)
        .collect();

    if corpus.is_empty() || queries.is_empty() {
        return Err("BEIR fixture has no benchmarkable corpus or queries".into());
    }

    Ok(Fixture {
        name: name.to_string(),
        corpus,
        queries,
        k: REAL_K,
    })
}

fn read_json(path: &Path) -> Result<JSONValue, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&data).map_err(|e| format!("{}: {e}", path.display()))
}

fn string_array(value: &JSONValue, key: &str) -> Result<Vec<String>, String> {
    value
        .get(key)
        .and_then(JSONValue::as_array)
        .ok_or_else(|| format!("missing JSON array {key}"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("non-string value in {key}"))
        })
        .collect()
}

fn number_array(value: &JSONValue, key: &str) -> Result<Vec<f64>, String> {
    value
        .get(key)
        .and_then(JSONValue::as_array)
        .ok_or_else(|| format!("missing JSON array {key}"))?
        .iter()
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| format!("non-number value in {key}"))
        })
        .collect()
}

fn read_npy_matrix(path: &Path) -> Result<Vec<Vec<f32>>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() < 12 || bytes[0..6] != [0x93, b'N', b'U', b'M', b'P', b'Y'] {
        return Err(format!("{} is not a NPY file", path.display()));
    }
    let major = bytes[6];
    let (header_len, data_offset) = if major <= 1 {
        let len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        (len, 10)
    } else {
        let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        (len, 12)
    };
    if bytes.len() < data_offset + header_len {
        return Err(format!("{} has a truncated NPY header", path.display()));
    }
    let header = std::str::from_utf8(&bytes[data_offset..data_offset + header_len])
        .map_err(|e| format!("{} header: {e}", path.display()))?;
    if header.contains("True") {
        return Err(format!("{} uses unsupported Fortran order", path.display()));
    }
    let descr = parse_descr(header)?;
    let (rows, cols) = parse_shape(header)?;
    let data = &bytes[data_offset + header_len..];
    let width = if descr.ends_with("f4") {
        4
    } else if descr.ends_with("f8") {
        8
    } else {
        return Err(format!("{} uses unsupported dtype {descr}", path.display()));
    };
    let expected_bytes = rows
        .checked_mul(cols)
        .and_then(|elements| elements.checked_mul(width))
        .ok_or_else(|| format!("{} has an overflowing NPY shape", path.display()))?;
    if data.len() != expected_bytes {
        return Err(format!(
            "{} NPY data length mismatch: expected {expected_bytes}, got {}",
            path.display(),
            data.len()
        ));
    }
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut values = Vec::with_capacity(cols);
        for col in 0..cols {
            let offset = (row * cols + col) * width;
            let value = if width == 4 {
                f32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ])
            } else {
                let value = f64::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                ]);
                if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
                    return Err(format!(
                        "{} value at ({row}, {col}) is outside f32 range: {value}",
                        path.display()
                    ));
                }
                value as f32
            };
            if !value.is_finite() {
                return Err(format!(
                    "{} value at ({row}, {col}) is not finite",
                    path.display()
                ));
            }
            values.push(value);
        }
        out.push(values);
    }
    Ok(out)
}

fn parse_descr(header: &str) -> Result<String, String> {
    for quote in ["'", "\""] {
        let needle = format!("'descr': {quote}");
        if let Some(start) = header.find(&needle) {
            let rest = &header[start + needle.len()..];
            if let Some(end) = rest.find(quote) {
                return Ok(rest[..end].to_string());
            }
        }
    }
    Err("missing NPY descr".into())
}

fn parse_shape(header: &str) -> Result<(usize, usize), String> {
    let start = header
        .find('(')
        .ok_or_else(|| "missing NPY shape".to_string())?;
    let end = header[start..]
        .find(')')
        .ok_or_else(|| "missing NPY shape end".to_string())?
        + start;
    let dims: Vec<usize> = header[start + 1..end]
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let trimmed = part.trim();
            trimmed
                .parse::<usize>()
                .map_err(|error| format!("invalid NPY shape dimension `{trimmed}`: {error}"))
        })
        .collect::<Result<_, _>>()?;
    if dims.len() == 2 {
        Ok((dims[0], dims[1]))
    } else {
        Err(format!("expected 2-D NPY shape, got {dims:?}"))
    }
}

fn ranked_dense(fx: &Fixture, query: &QueryCase) -> Vec<(usize, f64)> {
    let mut scored: Vec<(usize, f64)> = fx
        .corpus
        .iter()
        .enumerate()
        .map(|(doc_id, doc)| {
            (
                doc_id,
                VectorScorer::cosine_similarity(&query.vector, &doc.vector).unwrap(),
            )
        })
        .collect();
    sort_and_truncate(&mut scored, fx.k.min(fx.corpus.len()));
    scored
}

fn ranked_bm25(fx: &Fixture, query: &QueryCase) -> Vec<(usize, f64)> {
    let query_terms: BTreeSet<&str> = query.tokens.iter().map(String::as_str).collect();
    let n_docs = fx.corpus.len() as f64;
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for term in &query_terms {
        let count = fx
            .corpus
            .iter()
            .filter(|doc| doc.tokens.iter().any(|token| token == term))
            .count();
        df.insert(term, count.max(1));
    }
    let mut scored: Vec<(usize, f64)> = fx
        .corpus
        .iter()
        .enumerate()
        .map(|(doc_id, doc)| {
            let mut score = 0.0;
            for term in &query_terms {
                let tf = doc
                    .tokens
                    .iter()
                    .filter(|token| token.as_str() == *term)
                    .count() as f64;
                if tf > 0.0 {
                    let idf =
                        ((n_docs - df[term] as f64 + 0.5) / (df[term] as f64 + 0.5) + 1.0).ln();
                    score += idf * tf / (tf + 1.2);
                }
            }
            (doc_id, score)
        })
        .collect();
    sort_and_truncate(&mut scored, fx.k.min(fx.corpus.len()));
    scored
}

fn sort_and_truncate(scored: &mut Vec<(usize, f64)>, k: usize) {
    assert!(
        scored.iter().all(|(_, score)| score.is_finite()),
        "benchmark ranking produced a non-finite score"
    );
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(k);
}

fn normalize_scores(ranked: &[(usize, f64)]) -> BTreeMap<usize, f64> {
    if ranked.is_empty() {
        return BTreeMap::new();
    }
    let min_score = ranked
        .iter()
        .map(|(_, score)| *score)
        .fold(f64::INFINITY, f64::min);
    let max_score = ranked
        .iter()
        .map(|(_, score)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    ranked
        .iter()
        .map(|(doc_id, score)| {
            let p = if max_score > min_score {
                (*score - min_score) / (max_score - min_score)
            } else {
                0.5
            };
            (*doc_id, p.clamp(1e-10, 1.0 - 1e-10))
        })
        .collect()
}

fn reciprocal_rank_fusion(dense: &[(usize, f64)], sparse: &[(usize, f64)]) -> Vec<(usize, f64)> {
    let mut out = BTreeMap::new();
    for (rank, (doc_id, _)) in dense.iter().enumerate() {
        *out.entry(*doc_id).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    for (rank, (doc_id, _)) in sparse.iter().enumerate() {
        *out.entry(*doc_id).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    let mut ranked: Vec<(usize, f64)> = out.into_iter().collect();
    sort_and_truncate(&mut ranked, dense.len().max(sparse.len()));
    ranked
}

fn convex_fusion(
    dense_p: &BTreeMap<usize, f64>,
    sparse_p: &BTreeMap<usize, f64>,
) -> Vec<(usize, f64)> {
    let docs: BTreeSet<usize> = dense_p.keys().chain(sparse_p.keys()).copied().collect();
    let mut ranked: Vec<(usize, f64)> = docs
        .into_iter()
        .map(|doc_id| {
            let d = dense_p.get(&doc_id).copied().unwrap_or(0.0);
            let s = sparse_p.get(&doc_id).copied().unwrap_or(0.0);
            (doc_id, 0.5 * d + 0.5 * s)
        })
        .collect();
    let len = ranked.len();
    sort_and_truncate(&mut ranked, len);
    ranked
}

fn balanced_log_odds_fusion(
    dense_p: &BTreeMap<usize, f64>,
    sparse_p: &BTreeMap<usize, f64>,
) -> Vec<(usize, f64)> {
    let docs: BTreeSet<usize> = dense_p.keys().chain(sparse_p.keys()).copied().collect();
    let mut ranked: Vec<(usize, f64)> = docs
        .into_iter()
        .map(|doc_id| {
            let d = dense_p.get(&doc_id).copied().unwrap_or(1e-10);
            let s = sparse_p.get(&doc_id).copied().unwrap_or(1e-10);
            (doc_id, log_odds_conjunction(&[d, s], 0.5))
        })
        .collect();
    let len = ranked.len();
    sort_and_truncate(&mut ranked, len);
    ranked
}

fn distance_gap_weights(distances: &[f64]) -> Vec<f64> {
    if distances.len() < 2 {
        return vec![1.0; distances.len()];
    }
    let mut sorted = distances.to_vec();
    assert!(
        sorted.iter().all(|distance| distance.is_finite()),
        "distance-gap weighting received a non-finite distance"
    );
    sorted.sort_by(f64::total_cmp);
    let mut max_gap = 0.0;
    let mut gap_value = sorted[0];
    for pair in sorted.windows(2) {
        let gap = pair[1] - pair[0];
        if gap > max_gap {
            max_gap = gap;
            gap_value = pair[0];
        }
    }
    distances
        .iter()
        .map(|distance| {
            if *distance <= gap_value {
                1.0
            } else {
                (-(*distance - gap_value)).exp()
            }
        })
        .collect()
}

fn density_prior_weights(distances: &[f64]) -> Vec<f64> {
    if distances.is_empty() {
        return Vec::new();
    }
    let mean = distances.iter().sum::<f64>() / distances.len() as f64;
    let variance = distances
        .iter()
        .map(|distance| (*distance - mean).powi(2))
        .sum::<f64>()
        / distances.len() as f64;
    let sigma = variance.sqrt().max(1e-6);
    distances
        .iter()
        .map(|distance| (-((*distance - mean).powi(2)) / (2.0 * sigma * sigma)).exp())
        .collect()
}

fn calibration_weights(source: CalibrationSource, distances: &[f64], sparse_p: &[f64]) -> Vec<f64> {
    match source {
        CalibrationSource::DistanceGap => distance_gap_weights(distances),
        CalibrationSource::BayesianBM25 => sparse_p
            .iter()
            .map(|p| (0.25 + 0.75 * p).clamp(1e-10, 1.0))
            .collect(),
        CalibrationSource::DensityPrior => density_prior_weights(distances),
    }
}

fn calibrated_dense_report(fx: &Fixture, source: CalibrationSource) -> MethodReport {
    let calibrator = VectorProbabilityTransform::new(0.02, 0.55, 0.20, 0.05).unwrap();
    let mut naive_probs = Vec::new();
    let mut calibrated_probs = Vec::new();
    let mut labels = Vec::new();
    let mut ndcg_calibrated = 0.0;
    let mut map_calibrated = 0.0;
    let mut recall_calibrated = 0.0;
    for query in &fx.queries {
        let dense = ranked_dense(fx, query);
        let sparse_p = normalize_scores(&ranked_bm25(fx, query));
        let similarities: Vec<f64> = dense.iter().map(|(_, score)| *score).collect();
        let distances: Vec<f64> = similarities.iter().map(|score| 1.0 - *score).collect();
        let sparse_weights: Vec<f64> = dense
            .iter()
            .map(|(doc_id, _)| sparse_p.get(doc_id).copied().unwrap_or(1e-10))
            .collect();
        let weights = calibration_weights(source, &distances, &sparse_weights);
        let calibrated = calibrator.calibrate(&distances, Some(&weights)).unwrap();

        let mut calibrated_ranked: Vec<(usize, f64)> = dense
            .iter()
            .zip(&calibrated)
            .map(|((doc_id, _), p)| (*doc_id, *p))
            .collect();
        let cal_len = calibrated_ranked.len();
        sort_and_truncate(&mut calibrated_ranked, cal_len);

        ndcg_calibrated += ndcg_at_k(&relevances(query, &calibrated_ranked), METRIC_K);
        let relevant_flags: Vec<bool> = calibrated_ranked
            .iter()
            .take(METRIC_K)
            .map(|(doc_id, _)| query.relevant.contains_key(doc_id))
            .collect();
        let total_relevant = query.relevant.len().max(1);
        let hits = relevant_flags.iter().filter(|flag| **flag).count();
        map_calibrated += average_precision_at_k(&relevant_flags, total_relevant, METRIC_K);
        recall_calibrated += hits as f64 / total_relevant as f64;

        for ((doc_id, score), p_cal) in dense.iter().zip(&calibrated) {
            naive_probs.push(VectorScorer::similarity_to_probability(*score));
            calibrated_probs.push(*p_cal);
            labels.push(u8::from(query.relevant.contains_key(doc_id)));
        }
    }
    let q = fx.queries.len() as f64;
    MethodReport {
        ndcg: ndcg_calibrated / q,
        map: map_calibrated / q,
        recall: recall_calibrated / q,
        ece: CalibrationMetrics::ece(&calibrated_probs, &labels, 10).unwrap(),
        brier: CalibrationMetrics::brier(&calibrated_probs, &labels).unwrap(),
        log_loss: CalibrationMetrics::log_loss(&calibrated_probs, &labels).unwrap(),
    }
}

fn hybrid_reports(fx: &Fixture) -> BTreeMap<&'static str, MethodReport> {
    let mut per_method: BTreeMap<&'static str, Vec<QueryRun>> = BTreeMap::new();
    for query in &fx.queries {
        let dense = ranked_dense(fx, query);
        let bm25 = ranked_bm25(fx, query);
        let dense_p = normalize_scores(&dense);
        let sparse_p = normalize_scores(&bm25);
        let rrf = reciprocal_rank_fusion(&dense, &bm25);
        let convex = convex_fusion(&dense_p, &sparse_p);
        let balanced = balanced_log_odds_fusion(&dense_p, &sparse_p);
        for (name, ranked) in [
            ("Dense", dense),
            ("BM25", bm25),
            ("RRF", rrf),
            ("Convex", convex),
            ("Balanced", balanced),
        ] {
            per_method.entry(name).or_default().push(QueryRun {
                relevant: query.relevant.clone(),
                ranked,
            });
        }
    }
    per_method
        .into_iter()
        .map(|(name, runs)| (name, method_report(&runs)))
        .collect()
}

struct QueryRun {
    relevant: BTreeMap<usize, f64>,
    ranked: Vec<(usize, f64)>,
}

fn method_report(runs: &[QueryRun]) -> MethodReport {
    if runs.is_empty() {
        return MethodReport::default();
    }
    let mut ndcg = 0.0;
    let mut ap = 0.0;
    let mut recall = 0.0;
    let mut probs = Vec::new();
    let mut labels = Vec::new();
    for run in runs {
        let rel = relevances_from_map(&run.relevant, &run.ranked);
        let relevant_flags: Vec<bool> = run
            .ranked
            .iter()
            .take(METRIC_K)
            .map(|(doc_id, _)| run.relevant.contains_key(doc_id))
            .collect();
        let total_relevant = run.relevant.len().max(1);
        let hits = relevant_flags.iter().filter(|flag| **flag).count();
        ndcg += ndcg_at_k(&rel, METRIC_K);
        ap += average_precision_at_k(&relevant_flags, total_relevant, METRIC_K);
        recall += hits as f64 / total_relevant as f64;
        let score_probs = ranked_probabilities(&run.ranked);
        for ((doc_id, _), prob) in run.ranked.iter().zip(score_probs) {
            probs.push(prob);
            labels.push(u8::from(run.relevant.contains_key(doc_id)));
        }
    }
    let n = runs.len() as f64;
    MethodReport {
        ndcg: ndcg / n,
        map: ap / n,
        recall: recall / n,
        ece: CalibrationMetrics::ece(&probs, &labels, 10).unwrap(),
        brier: CalibrationMetrics::brier(&probs, &labels).unwrap(),
        log_loss: CalibrationMetrics::log_loss(&probs, &labels).unwrap(),
    }
}

fn relevances(query: &QueryCase, ranked: &[(usize, f64)]) -> Vec<f64> {
    relevances_from_map(&query.relevant, ranked)
}

fn relevances_from_map(relevant: &BTreeMap<usize, f64>, ranked: &[(usize, f64)]) -> Vec<f64> {
    ranked
        .iter()
        .map(|(doc_id, _)| relevant.get(doc_id).copied().unwrap_or(0.0))
        .collect()
}

fn ranked_probabilities(ranked: &[(usize, f64)]) -> Vec<f64> {
    if ranked.is_empty() {
        return Vec::new();
    }
    let min_score = ranked
        .iter()
        .map(|(_, score)| *score)
        .fold(f64::INFINITY, f64::min);
    let max_score = ranked
        .iter()
        .map(|(_, score)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    ranked
        .iter()
        .map(|(_, score)| {
            if max_score > min_score {
                ((*score - min_score) / (max_score - min_score)).clamp(1e-10, 1.0 - 1e-10)
            } else {
                0.5
            }
        })
        .collect()
}

fn report_checksum(reports: &BTreeMap<&'static str, MethodReport>) -> f64 {
    reports
        .values()
        .map(|r| r.ndcg + r.map + r.recall + r.ece + r.brier + r.log_loss)
        .sum()
}

fn bench_gap_calibration_sources(c: &mut Criterion) {
    let fx = fixture();
    let mut group = c.benchmark_group("beir_calibration_sources");
    for (name, source) in [
        ("distance_gap", CalibrationSource::DistanceGap),
        ("bayesian_bm25", CalibrationSource::BayesianBM25),
        ("density_prior", CalibrationSource::DensityPrior),
    ] {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &source,
            |bencher, source| {
                bencher.iter(|| {
                    let report = calibrated_dense_report(black_box(&fx), black_box(*source));
                    black_box(report.ndcg + report.map + report.recall + report.ece)
                });
            },
        );
    }
    group.finish();
}

fn bench_hybrid_method_report(c: &mut Criterion) {
    let fx = fixture();
    c.bench_function("beir_hybrid_method_report", |bencher| {
        bencher.iter(|| {
            let reports = hybrid_reports(black_box(&fx));
            black_box(report_checksum(&reports))
        });
    });
}

fn bench_real_fixture_loader(c: &mut Criterion) {
    c.bench_function("beir_real_fixture_probe", |bencher| {
        bencher.iter(|| black_box(load_real_beir_fixture().map(|fx| (fx.name, fx.queries.len()))));
    });
}

criterion_group!(
    benches,
    bench_gap_calibration_sources,
    bench_hybrid_method_report,
    bench_real_fixture_loader
);
criterion_main!(benches);
