//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative cancellation return latency, cleanup, and recovery through public owner APIs.

use super::{allocation, Case, Corpus, CORPUS};
use allocation_counter::{measure, opt_out};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::hint::black_box;
use std::time::Instant;
use uqa_analysis::nori::{
    DecompoundMode, KoreanAnalyzer, KoreanTokenizer, NoriLimits, NoriOptions, NoriOutput,
    NoriResources,
};
use uqa_analysis::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

const SAMPLES: usize = 7;
const WARMUP: usize = 2;
const ITERATIONS: usize = 16;
const ALLOWANCE: usize = 256 * 1024 * 1024;
const RETAINED: usize = 4096;

enum Runner<'a> {
    Tokenizer(&'a KoreanTokenizer),
    Analyzer(&'a KoreanAnalyzer),
}

impl Runner<'_> {
    fn run(
        &self,
        text: &str,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<NoriOutput>> {
        match self {
            Self::Tokenizer(value) => {
                value.tokenize_budgeted(text, NoriLimits::default(), budget, poll)
            }
            Self::Analyzer(value) => {
                value.analyze_budgeted(text, NoriLimits::default(), budget, poll)
            }
        }
    }

    fn full(&self, text: &str) -> NoriOutput {
        match self {
            Self::Tokenizer(value) => value.tokenize(text),
            Self::Analyzer(value) => value.analyze(text),
        }
        .expect("complete reference analysis")
    }
}

fn cancel(runner: &Runner<'_>, text: &str, budget: &MemoryBudget, at: usize) -> (u64, u64) {
    let mut polls = 0;
    let mut requested = None;
    let started = Instant::now();
    let result = runner.run(black_box(text), budget, &mut || {
        polls += 1;
        if polls == at {
            requested = Some(Instant::now());
            Err(AnalysisError::Cancelled)
        } else {
            Ok(())
        }
    });
    let returned = Instant::now();
    let requested = requested.expect("the selected poll must be reached");
    assert!(matches!(result, Err(AnalysisError::Cancelled)));
    assert_eq!(polls, at, "no work may continue polling after cancellation");
    assert_eq!(
        budget.used(),
        RETAINED,
        "preserve the unrelated reservation"
    );
    (
        returned.duration_since(started).as_nanos() as u64,
        returned.duration_since(requested).as_nanos() as u64,
    )
}

fn recover(runner: &Runner<'_>, text: &str, budget: &MemoryBudget, expected: &NoriOutput) {
    let output = runner.run(text, budget, &mut || Ok(())).expect("recovery");
    assert_eq!(&*output, expected, "complete output after cancellation");
    drop(output);
    assert_eq!(budget.used(), RETAINED);
}

fn timing(elapsed_ns: &[u64]) -> Value {
    let mut sorted = elapsed_ns.to_vec();
    sorted.sort_unstable();
    json!({
        "elapsed_ns": elapsed_ns,
        "iterations": ITERATIONS,
        "median_ns": sorted[SAMPLES / 2] as f64 / ITERATIONS as f64,
    })
}

fn cases(case: &Case, stage: &str, mode: DecompoundMode, runner: &Runner<'_>) -> Vec<Value> {
    let expected = runner.full(&case.text);
    let output_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&expected).unwrap())
    );
    let budget = MemoryBudget::new(ALLOWANCE);
    let retained = budget.reserve(RETAINED).unwrap();
    let mut complete_polls = 0_usize;
    let output = runner
        .run(&case.text, &budget, &mut || {
            complete_polls += 1;
            Ok(())
        })
        .expect("count complete analysis polls");
    assert_eq!(&*output, &expected);
    drop(output);
    assert_eq!(budget.used(), RETAINED);
    assert!(
        complete_polls >= 3,
        "distinct beginning, middle, and final polls"
    );
    let mut results = Vec::new();
    for (point, at) in [
        ("first", 1),
        ("middle", complete_polls.div_ceil(2)),
        ("last", complete_polls),
    ] {
        let mut operation = Vec::with_capacity(SAMPLES);
        let mut response = Vec::with_capacity(SAMPLES);
        opt_out(|| {
            for _ in 0..WARMUP {
                black_box(cancel(runner, &case.text, &budget, at));
                recover(runner, &case.text, &budget, &expected);
            }
            for _ in 0..SAMPLES {
                let mut total = 0;
                let mut propagation = 0;
                for _ in 0..ITERATIONS {
                    let (elapsed, returned) = cancel(runner, &case.text, &budget, at);
                    total += elapsed;
                    propagation += returned;
                }
                operation.push(total);
                response.push(propagation);
                recover(runner, &case.text, &budget, &expected);
            }
        });
        let info = measure(|| {
            black_box(cancel(runner, &case.text, &budget, at));
        });
        assert_eq!(info.bytes_current, 0);
        assert_eq!(info.count_current, 0);
        recover(runner, &case.text, &budget, &expected);
        results.push(json!({
            "name": format!("{stage}/{mode:?}/{}/{point}", case.name),
            "input_bytes": case.text.len(),
            "input_utf16": case.text.encode_utf16().count(),
            "input_sha256": format!("{:x}", Sha256::digest(case.text.as_bytes())),
            "output_sha256": output_sha256,
            "tokens": expected.tokens.len(),
            "complete_polls": complete_polls,
            "cancel_at_poll": at,
            "verified_cancellations": WARMUP + SAMPLES * ITERATIONS + 1,
            "verified_recoveries": WARMUP + SAMPLES + 1,
            "remaining_budget_bytes": budget.used(),
            "allocation": allocation(info),
            "operation_timing": timing(&operation),
            "response_timing": timing(&response),
        }));
        eprintln!(
            "measured cancellation {stage}/{mode:?}/{}/{point}",
            case.name
        );
    }
    drop(retained);
    assert_eq!(budget.used(), 0);
    results
}

pub(super) fn run() -> Value {
    let resources = NoriResources::default();
    let resolved = resources.load_default().expect("bundled dictionary");
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("fixed corpus");
    let mut results = Vec::new();
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
            results.extend(cases(
                &case,
                "tokenizer",
                mode,
                &Runner::Tokenizer(&tokenizer),
            ));
            results.extend(cases(&case, "analyzer", mode, &Runner::Analyzer(&analyzer)));
        }
    }
    json!({
        "schema_version": 1,
        "owner": "uqa-analysis",
        "purpose": "cooperative_cancellation",
        "target_arch": std::env::consts::ARCH,
        "target_os": std::env::consts::OS,
        "pointer_bits": usize::BITS,
        "threads": 1,
        "protocol": {"samples": SAMPLES, "warmup": WARMUP, "timed_operations_per_sample": ITERATIONS, "allocation_samples": 1, "clock": "per_operation"},
        "memory_limit_bytes": ALLOWANCE,
        "unrelated_reservation_bytes": RETAINED,
        "bundle_bytes": uqa_nori_data::BUNDLE.len(),
        "bundle_sha256": uqa_nori_data::BUNDLE_SHA256,
        "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "timing_scope": "operation: entry to cancelled return; response: callback decision to cancelled return, including workspace destruction and clock-call overhead; excludes verification and subsequent recovery; no external-signal detection or scheduler latency claim",
        "allocation_scope": "current-thread Rust System allocator requests during one cancelled operation; excludes preloaded dictionary, input/reference output, allowance handle, unrelated reservation, and host memory",
        "measurements": results,
    })
}
