//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nori phrase analysis, graph matching and scoring through their owning public APIs.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use allocation_counter::{measure, opt_out};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::{
    nori::{nori_analyzer, DecompoundMode},
    whitespace_analyzer, AnalysisError, CompiledAnalyzer, Tokenizer,
};
use uqa_core::{memory::Budgeted, memory::MemoryBudget, CancellationToken, ScoredEntry};
use uqa_operators::phrase::{score_phrase_budgeted, PhraseBudget};
use uqa_scoring::{BM25Params, ScoringMode};
use uqa_storage::{
    inverted_index::{analyze_query_graph, analyze_query_graph_budgeted},
    AnalyzerPhase, InvertedIndex, MemoryInvertedIndex,
};

const CORPUS: &str = include_str!("../../uqa-analysis/benches/nori/corpus.json");
const DOCUMENTS: u64 = 2048;
const SAMPLES: usize = 7;
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

fn fixture(cases: &[Value], revision: &Arc<CompiledAnalyzer>) -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(whitespace_analyzer());
    index
        .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
        .unwrap();
    let texts: Vec<_> = cases
        .iter()
        .map(|case| {
            case["text"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap() as usize)
        })
        .collect();
    for id in 0..DOCUMENTS + cases.len() as u64 {
        let text = if id < DOCUMENTS {
            texts[id as usize % cases.len()].as_str()
        } else {
            cases[(id - DOCUMENTS) as usize]["text"].as_str().unwrap()
        };
        index
            .add_document(id, BTreeMap::from([("body".into(), text.into())]))
            .unwrap();
    }
    index
}

fn probe(
    name: &str,
    case_index: usize,
    cases: usize,
    allocation_only: bool,
    run: impl Fn() -> Budgeted<Vec<ScoredEntry>>,
) -> Value {
    let mut counted = None;
    let first = if allocation_only {
        let mut retained = None;
        counted = Some(measure(|| retained = Some(run())));
        retained.unwrap()
    } else {
        run()
    };
    assert!(first
        .iter()
        .any(|row| row.doc_id == DOCUMENTS + case_index as u64));
    assert!(first.windows(2).all(|pair| pair[0].doc_id < pair[1].doc_id));
    for row in first.iter() {
        assert!(row.score.is_finite() && row.score > 0.0);
        assert!(
            (row.doc_id < DOCUMENTS && row.doc_id as usize % cases == case_index)
                || row.doc_id == DOCUMENTS + case_index as u64
        );
    }
    let expected: Vec<_> = first
        .iter()
        .map(|row| (row.doc_id, row.score.to_bits()))
        .collect();
    drop(first);
    let verify = |rows: &[ScoredEntry]| {
        assert_eq!(rows.len(), expected.len());
        for (row, &(id, score)) in rows.iter().zip(&expected) {
            assert_eq!(row.doc_id, id);
            assert_eq!(row.score.to_bits(), score);
        }
    };
    let mut elapsed_ns = Vec::new();
    if !allocation_only {
        elapsed_ns.reserve(SAMPLES);
        opt_out(|| {
            for _ in 0..SAMPLES {
                let start = Instant::now();
                let rows = black_box(run());
                elapsed_ns.push(start.elapsed().as_nanos() as u64);
                verify(&rows);
            }
        });
    }
    let info = counted.unwrap_or_else(|| {
        let mut retained = None;
        let info = measure(|| retained = Some(run()));
        verify(retained.as_ref().unwrap());
        info
    });
    eprintln!("measured {name}");
    let mut row = json!({
        "name": name,
        "verified_samples": if allocation_only { 1 } else { SAMPLES + 2 },
        "allocation": {
            "count_total": info.count_total, "count_peak": info.count_max, "count_retained": info.count_current,
            "bytes_total": info.bytes_total, "bytes_peak": info.bytes_max, "bytes_retained": info.bytes_current,
        },
        "rows": expected.iter().map(|&(id, bits)| json!([id, f64::from_bits(bits)])).collect::<Vec<_>>(),
    });
    if !allocation_only {
        let mut ordered = elapsed_ns.clone();
        ordered.sort_unstable();
        row["elapsed_ns"] = json!(elapsed_ns);
        row["median_ns"] = json!(ordered[SAMPLES / 2]);
    }
    row
}

fn main() {
    let allocation_only = std::env::args_os().any(|argument| argument == "--allocation-only");
    let corpus: Value = serde_json::from_str(CORPUS).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    let cancellation = CancellationToken::new();
    let scoring = ScoringMode::BM25(BM25Params::default());
    let mut measurements = Vec::new();
    for mode in [
        DecompoundMode::None,
        DecompoundMode::Discard,
        DecompoundMode::Mixed,
    ] {
        let mut config = nori_analyzer();
        let Tokenizer::Nori(tokenizer) = &mut config.tokenizer else {
            unreachable!()
        };
        tokenizer.decompound_mode = mode;
        let revision = config.compile().unwrap();
        let index = fixture(cases, &revision);
        for (case_index, case) in cases.iter().enumerate() {
            let text = case["text"].as_str().unwrap();
            let query = analyze_query_graph(&revision, text).unwrap();
            assert!(!query.is_empty());
            for analysis in [false, true] {
                let stage = if analysis {
                    "analysis_and_match"
                } else {
                    "match_graph"
                };
                let name = format!("{stage}/{mode:?}/{}", case["name"].as_str().unwrap());
                let run = || {
                    let memory = MemoryBudget::new(MEMORY_LIMIT);
                    let budget = PhraseBudget::with_memory(&memory, &cancellation);
                    if analysis {
                        let query = analyze_query_graph_budgeted(&revision, text, &memory, || {
                            cancellation.check().map_err(|_| AnalysisError::Cancelled)
                        })
                        .unwrap();
                        score_phrase_budgeted(&index, "body", &query, &scoring, &budget).unwrap()
                    } else {
                        score_phrase_budgeted(&index, "body", &query, &scoring, &budget).unwrap()
                    }
                };
                let mut row = probe(&name, case_index, cases.len(), allocation_only, run);
                row["query_occurrences"] = json!(query.len());
                row["query_sha256"] = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
                row["analyzer_fingerprint"] =
                    json!(revision.descriptor().fingerprint().to_string());
                measurements.push(row);
            }
        }
    }
    let mut report = json!({
        "schema_version": 1, "owner": "uqa-operators", "target_arch": std::env::consts::ARCH,
        "target_os": std::env::consts::OS, "pointer_bits": usize::BITS, "threads": 1,
        "documents": DOCUMENTS + cases.len() as u64, "memory_limit": MEMORY_LIMIT,
        "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "allocation_scope": "current-thread Rust allocator requests with scored results retained; excludes dictionary, index, pre-analyzed match_graph input, stack and host heap",
        "measurements": measurements,
    });
    report["protocol"] = if allocation_only {
        json!({"allocation_samples": 1, "samples": 0, "warmup": 0, "timed_operations_per_sample": 0})
    } else {
        report["timing_scope"] = json!("query-owned budgets, graph matching and BM25 scoring; analysis_and_match also includes complete phrase analysis and lossless key projection; excludes dictionary/index setup, verification and final result drop");
        json!({"samples": SAMPLES, "warmup": 1, "timed_operations_per_sample": 1})
    };
    println!("{report}");
}
