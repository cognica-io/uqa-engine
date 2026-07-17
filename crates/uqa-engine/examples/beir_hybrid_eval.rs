//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hybrid relevance evaluation on a real BEIR dataset with real
//! sentence-transformer embeddings.
//!
//! Prepare the data once with `tests/beir/encode_beir.py` (downloads a
//! BEIR dataset and encodes it with all-MiniLM-L6-v2), then run:
//!
//! ```text
//! cargo run --release -p uqa-engine --example beir_hybrid_eval -- <data-dir> [dump]
//! ```
//!
//! Reports NDCG@10 (normalized by the true ideal over the full qrels,
//! so recall misses cost score), MAP@10, and Recall@10 for text-only,
//! vector-only, and hybrid retrieval, plus the hybrid score
//! distribution across queries. The optional `dump` argument prints
//! per-query hybrid NDCG lines for paired significance tests.
//!
//! Reference points measured on `SciFact` (5,183 docs, 300 test
//! queries, `all-MiniLM-L6-v2`): text 0.6860, vector 0.6451, hybrid
//! 0.7236 `NDCG@10` -- in line with published BM25 (~0.67), `MiniLM`
//! (~0.65), and hybrid (~0.70-0.72) baselines, and above the Lucene
//! PR 15948 configuration (median-beta estimator with a per-signal
//! prior), which measures 0.7079 on the same data.

use std::collections::BTreeMap;
use std::io::BufRead;

use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams, ScoringMode};
use uqa_scoring::BM25Params;
use uqa_storage::document_store::Document;

const K: usize = 10;
const KNN_POOL: usize = 100;

struct QueryCase {
    text: String,
    embedding: Vec<f32>,
    judgments: BTreeMap<u64, f64>,
}

fn json_field<'a>(value: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}

fn dcg(relevances: &[f64], k: usize) -> f64 {
    relevances
        .iter()
        .take(k)
        .enumerate()
        .map(|(rank, gain)| gain / ((rank + 2) as f64).log2())
        .sum()
}

fn true_ideal_ndcg(relevances: &[f64], judgments: &BTreeMap<u64, f64>, k: usize) -> f64 {
    let mut ideal: Vec<f64> = judgments.values().copied().collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let idcg = dcg(&ideal, k);
    if idcg == 0.0 {
        return 0.0;
    }
    dcg(relevances, k) / idcg
}

fn average_precision(relevances: &[f64], total_relevant: usize, k: usize) -> f64 {
    if total_relevant == 0 {
        return 0.0;
    }
    let mut hits = 0.0;
    let mut sum = 0.0;
    for (rank, gain) in relevances.iter().take(k).enumerate() {
        if *gain > 0.0 {
            hits += 1.0;
            sum += hits / (rank + 1) as f64;
        }
    }
    sum / total_relevant.min(k) as f64
}

#[allow(clippy::too_many_lines)]
fn main() {
    let data_dir = std::env::args()
        .nth(1)
        .expect("usage: beir_hybrid_eval <data-dir>");

    let engine = Engine::new();
    engine.create_default_table("docs", vec!["body".into()]);
    engine.create_vector_field("docs", "embedding", 384);

    let corpus_file = std::fs::File::open(format!("{data_dir}/eval_corpus.jsonl")).unwrap();
    let mut doc_count = 0u64;
    for line in std::io::BufReader::new(corpus_file).lines() {
        let row: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id: u64 = json_field(&row, "id").as_str().unwrap().parse().unwrap();
        let body = json_field(&row, "body").as_str().unwrap().to_string();
        let embedding: Vec<f32> = json_field(&row, "embedding")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        let mut d = Document::new();
        d.insert("body".into(), Value::Str(body));
        let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
        vectors.insert("embedding".into(), embedding);
        engine
            .add_document_with_vectors("docs", id, d, vectors)
            .unwrap();
        doc_count += 1;
    }

    let query_file = std::fs::File::open(format!("{data_dir}/eval_queries.jsonl")).unwrap();
    let mut queries: Vec<QueryCase> = Vec::new();
    for line in std::io::BufReader::new(query_file).lines() {
        let row: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let judgments: BTreeMap<u64, f64> = json_field(&row, "judgments")
            .as_object()
            .unwrap()
            .iter()
            .map(|(doc, score)| (doc.parse::<u64>().unwrap(), score.as_f64().unwrap()))
            .collect();
        queries.push(QueryCase {
            text: json_field(&row, "text").as_str().unwrap().to_string(),
            embedding: json_field(&row, "embedding")
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect(),
            judgments,
        });
    }
    eprintln!("loaded {doc_count} docs, {} queries", queries.len());

    let evaluate = |name: &str, ranked_ids_for: &dyn Fn(&QueryCase) -> Vec<u64>| {
        let mut ndcg_sum = 0.0;
        let mut map_sum = 0.0;
        let mut recall_sum = 0.0;
        let mut top_scores: Vec<f64> = Vec::new();
        for query in &queries {
            let ranked = ranked_ids_for(query);
            let relevances: Vec<f64> = ranked
                .iter()
                .map(|id| query.judgments.get(id).copied().unwrap_or(0.0))
                .collect();
            ndcg_sum += true_ideal_ndcg(&relevances, &query.judgments, K);
            let total_relevant = query.judgments.values().filter(|g| **g > 0.0).count();
            map_sum += average_precision(&relevances, total_relevant, K);
            let found = relevances.iter().take(K).filter(|g| **g > 0.0).count();
            recall_sum += found as f64 / total_relevant.max(1) as f64;
            let _ = &mut top_scores;
        }
        let n = queries.len() as f64;
        println!(
            "{name}\t{:.4}\t{:.4}\t{:.4}",
            ndcg_sum / n,
            map_sum / n,
            recall_sum / n,
        );
    };

    let hybrid_scores = |query: &QueryCase| -> Vec<(u64, f64)> {
        engine
            .hybrid_search(&HybridSearchParams {
                table: "docs",
                text_field: "body",
                text_query: &query.text,
                vector_field: "embedding",
                query_vector: query.embedding.clone(),
                knn_pool: KNN_POOL,
                alpha: 0.5,
                top_k: K,
            })
            .into_iter()
            .map(|hit| (hit.doc_id, hit.score))
            .collect()
    };

    println!("system\tNDCG@{K}\tMAP@{K}\tRecall@{K}");
    evaluate("text_bm25", &|query| {
        engine
            .search(
                "docs",
                "body",
                &query.text,
                &ScoringMode::BM25(BM25Params::default()),
                K,
            )
            .into_iter()
            .map(|hit| hit.doc_id)
            .collect()
    });
    evaluate("vector_only", &|query| {
        engine
            .knn_search("docs", "embedding", &query.embedding, K)
            .into_iter()
            .map(|hit| hit.doc_id)
            .collect()
    });
    evaluate("hybrid", &|query| {
        hybrid_scores(query).into_iter().map(|(id, _)| id).collect()
    });

    if std::env::args().nth(2).as_deref() == Some("dump") {
        for (index, query) in queries.iter().enumerate() {
            let ranked: Vec<u64> = hybrid_scores(query).into_iter().map(|(id, _)| id).collect();
            let relevances: Vec<f64> = ranked
                .iter()
                .map(|id| query.judgments.get(id).copied().unwrap_or(0.0))
                .collect();
            println!(
                "query\t{index}\t{:.6}",
                true_ideal_ndcg(&relevances, &query.judgments, K)
            );
        }
    }

    // Hybrid score distribution across queries: are fused scores
    // spread out and thresholdable, or piled at the ceiling?
    let mut top1: Vec<f64> = Vec::new();
    let mut top10: Vec<f64> = Vec::new();
    for query in &queries {
        let scored = hybrid_scores(query);
        if let Some((_, score)) = scored.first() {
            top1.push(*score);
        }
        if let Some((_, score)) = scored.last() {
            top10.push(*score);
        }
    }
    top1.sort_by(f64::total_cmp);
    top10.sort_by(f64::total_cmp);
    let pct = |values: &[f64], p: f64| values[(values.len() as f64 * p) as usize];
    println!(
        "hybrid top-1 score: p10={:.3} median={:.3} p90={:.3} max={:.3}",
        pct(&top1, 0.10),
        pct(&top1, 0.50),
        pct(&top1, 0.90),
        top1[top1.len() - 1],
    );
    println!(
        "hybrid rank-{K} score: p10={:.3} median={:.3} p90={:.3}",
        pct(&top10, 0.10),
        pct(&top10, 0.50),
        pct(&top10, 0.90),
    );
}
