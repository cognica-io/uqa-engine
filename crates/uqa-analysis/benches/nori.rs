//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-threaded native/WASM measurements through the analysis owner's public API.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use allocation_counter::{measure, opt_out, AllocationInfo};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::nori::{
    DecompoundMode, DictionaryLimits, DictionaryRequest, KoreanAnalyzer, KoreanTokenizer,
    NoriDictionary, NoriOptions, NoriOutput, NoriResources, DEFAULT_NORI_DICTIONARY,
};

#[path = "nori/cancellation.rs"]
mod cancellation;

const CORPUS: &str = include_str!("nori/corpus.json");
const SAMPLES: usize = 7;
const WARMUP: usize = 2;
const SAMPLE_TIME: Duration = Duration::from_millis(75);

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    text: String,
    repeat: usize,
}

fn allocation(info: AllocationInfo) -> Value {
    json!({
        "count_total": info.count_total,
        "count_retained": info.count_current,
        "count_peak": info.count_max,
        "bytes_total": info.bytes_total,
        "bytes_retained": info.bytes_current,
        "bytes_peak": info.bytes_max,
    })
}

// Suppress counter updates during timing; allocator dispatch still has its opt-out check.
fn timing<T>(mut run: impl FnMut() -> T, samples: usize) -> Value {
    let mut result = Value::Null;
    opt_out(|| {
        for _ in 0..WARMUP {
            drop(black_box(run()));
        }
        let pilot = Instant::now();
        drop(black_box(run()));
        let batch =
            (SAMPLE_TIME.as_nanos() / pilot.elapsed().as_nanos().max(1)).clamp(1, 1_000_000) as u64;
        let mut durations = Vec::with_capacity(samples);
        let mut iterations = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            let mut count = 0_u64;
            loop {
                for _ in 0..batch {
                    drop(black_box(run()));
                }
                count += batch;
                if start.elapsed() >= SAMPLE_TIME {
                    break;
                }
            }
            durations.push(start.elapsed().as_nanos() as u64);
            iterations.push(count);
        }
        let mut ns: Vec<f64> = durations
            .iter()
            .zip(&iterations)
            .map(|(&elapsed, &count)| elapsed as f64 / count as f64)
            .collect();
        ns.sort_by(f64::total_cmp);
        result = json!({
            "elapsed_ns": durations,
            "iterations": iterations,
            "median_ns": ns[ns.len() / 2],
        });
    });
    result
}

fn cold_dictionary(allocation_only: bool) -> Value {
    let decode = || {
        NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())
            .expect("decode pinned dictionary")
    };
    let timings = (!allocation_only).then(|| timing(decode, 3));
    let mut retained = None;
    let info = measure(|| retained = Some(decode()));
    let model = retained.take().expect("retained dictionary");
    assert_eq!(model.known_word_count(), 816_283);
    assert_eq!(model.surface_count(), 774_582);
    drop(model);
    let mut row = json!({"name": "cold_decode_validate_drop", "allocation": allocation(info)});
    if let Some(timings) = timings {
        row["timing"] = timings;
    }
    row
}

fn shared_dictionary(
    resources: &NoriResources,
    request: &DictionaryRequest,
    name: &str,
    allocation_only: bool,
) -> Value {
    let first = resources.load_default().expect("cached dictionary");
    let mut handles = Vec::with_capacity(64);
    let info = measure(|| {
        for _ in 0..64 {
            let handle = resources.load(request).expect("shared dictionary");
            assert!(Arc::ptr_eq(first.model(), handle.model()));
            handles.push(handle);
        }
    });
    assert_eq!(info.bytes_current, 0);
    assert_eq!(info.count_current, 0);
    let mut row = json!({
        "name": name,
        "handles": handles.len(),
        "allocation": allocation(info),
    });
    if !allocation_only {
        row["timing"] = timing(
            || resources.load(request).expect("cached dictionary"),
            SAMPLES,
        );
    }
    row
}

fn analyze_case(
    case: &Case,
    stage: &str,
    mode: DecompoundMode,
    allocation_only: bool,
    run: impl Fn() -> NoriOutput,
) -> Value {
    let output = run();
    let output_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&output).unwrap()));
    let tokens = output.tokens.len();
    drop(output);
    let info = measure(|| drop(black_box(run())));
    assert_eq!(info.bytes_current, 0, "{stage}/{} leaked bytes", case.name);
    assert_eq!(
        info.count_current, 0,
        "{stage}/{} leaked allocations",
        case.name
    );
    let mut row = json!({
        "name": format!("{stage}/{mode:?}/{}", case.name),
        "input_bytes": case.text.len(),
        "input_utf16": case.text.encode_utf16().count(),
        "input_sha256": format!("{:x}", Sha256::digest(case.text.as_bytes())),
        "output_sha256": output_sha256,
        "tokens": tokens,
        "allocation": allocation(info),
    });
    if !allocation_only {
        row["timing"] = timing(run, SAMPLES);
    }
    row
}

fn main() {
    if std::env::args_os().any(|argument| argument == "--cancellation") {
        println!("{}", cancellation::run());
        return;
    }
    let allocation_only = std::env::args_os().any(|argument| argument == "--allocation-only");
    let mut results = vec![cold_dictionary(allocation_only)];
    eprintln!("measured cold dictionary loading");
    let resources = NoriResources::default();
    let resolved = resources.load_default().expect("bundled dictionary");
    results.push(shared_dictionary(
        &resources,
        &DictionaryRequest::Name(DEFAULT_NORI_DICTIONARY.into()),
        "cached_name_resolve_64_shared_handles",
        allocation_only,
    ));
    results.push(shared_dictionary(
        &resources,
        &DictionaryRequest::Sha256(resolved.sha256()),
        "cached_identity_resolve_64_shared_handles",
        allocation_only,
    ));
    eprintln!("measured shared dictionary resolution");
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("fixed corpus");
    for mut case in corpus.cases {
        case.text = case.text.repeat(case.repeat);
        for mode in [
            DecompoundMode::None,
            DecompoundMode::Discard,
            DecompoundMode::Mixed,
        ] {
            let options = NoriOptions {
                decompound_mode: mode,
                ..NoriOptions::default()
            };
            let tokenizer = KoreanTokenizer::new(resolved.model().clone(), None, options).unwrap();
            let analyzer = KoreanAnalyzer::new(resolved.model().clone(), None, options).unwrap();
            results.push(analyze_case(
                &case,
                "tokenizer",
                mode,
                allocation_only,
                || {
                    tokenizer
                        .tokenize(black_box(&case.text))
                        .expect("tokenization")
                },
            ));
            results.push(analyze_case(
                &case,
                "analyzer",
                mode,
                allocation_only,
                || analyzer.analyze(black_box(&case.text)).expect("analysis"),
            ));
        }
        eprintln!("measured {}", case.name);
    }
    let mut report = json!({
        "schema_version": 1,
        "target_arch": std::env::consts::ARCH,
        "target_os": std::env::consts::OS,
        "pointer_bits": usize::BITS,
        "threads": 1,
        "allocation_counter": "0.8.1; current-thread Rust System allocator requests; excludes stack, static bundle, allocator metadata and host JS heap",
        "bundle_bytes": uqa_nori_data::BUNDLE.len(),
        "bundle_sha256": uqa_nori_data::BUNDLE_SHA256,
        "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "measurements": results,
    });
    report["protocol"] = if allocation_only {
        json!({"allocation_samples": 1, "samples": 0, "warmup": 0, "timed_operations_per_sample": 0})
    } else {
        report["timing_scope"] = json!(
            "create and drop each result; warmed code and input pages; counter updates disabled"
        );
        json!({"samples": SAMPLES, "cold_samples": 3, "warmup": WARMUP, "pilot": 1, "clock": "per_batch", "sample_ms": SAMPLE_TIME.as_millis()})
    };
    println!("{report}");
}
