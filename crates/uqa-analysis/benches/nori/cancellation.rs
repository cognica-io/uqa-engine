//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cooperative cancellation return latency, cleanup, and recovery through public owner APIs.

use super::{Case, Corpus, CORPUS};
use allocation_counter::{measure, opt_out, AllocationInfo};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};
use uqa_analysis::nori::{
    DecompoundMode, KoreanAnalyzer, KoreanTokenizer, NoriLimits, NoriOptions, NoriOutput,
    NoriResources,
};
use uqa_analysis::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

const SAMPLES: usize = 7;
const WARMUP: usize = 2;
const BATCH: usize = 16;
const SAMPLE_TIME: Duration = Duration::from_millis(75);
const ALLOWANCE: usize = 256 * 1024 * 1024;
const RETAINED: usize = 4096;

#[derive(Default)]
struct Sampling {
    iterations: Option<BTreeMap<String, usize>>,
    fixed: bool,
    diagnostic: bool,
    identity: Option<String>,
}

impl Sampling {
    fn load() -> Self {
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            if argument == "--cancellation-fixed-iterations" {
                let input = arguments.next().expect("fixed iteration plan argument");
                return Self::from_bytes(input.as_bytes(), true, false);
            }
        }
        let Some(path) = std::env::var_os("UQA_NORI_CANCELLATION_SAMPLING") else {
            return Self::default();
        };
        let fixed = match std::env::var("UQA_NORI_CANCELLATION_SAMPLING_MODE").as_deref() {
            Ok("fixed") => true,
            Ok("timed") => false,
            _ => panic!("sampling control requires a fixed or timed mode"),
        };
        let bytes = std::fs::read(path).expect("sampling control input");
        Self::from_bytes(&bytes, fixed, true)
    }

    fn from_bytes(bytes: &[u8], fixed: bool, diagnostic: bool) -> Self {
        let iterations: BTreeMap<String, usize> =
            serde_json::from_slice(bytes).expect("sampling control iterations");
        assert_eq!(iterations.len(), 108, "complete sampling control coverage");
        assert!(iterations
            .values()
            .all(|count| *count > 0 && count.is_multiple_of(BATCH)));
        Self {
            iterations: Some(iterations),
            fixed,
            diagnostic,
            identity: Some(format!("{:x}", Sha256::digest(bytes))),
        }
    }

    fn limit(&self, stage: &str, mode: DecompoundMode, case: &Case, point: &str) -> Option<usize> {
        let iterations = self.iterations.as_ref()?;
        let name = format!("{stage}/{mode:?}/{}/{point}", case.name);
        let count = *iterations.get(&name).expect("sampling control workload");
        self.fixed.then_some(count)
    }

    fn protocol(&self) -> Value {
        let mut protocol = json!({"samples": SAMPLES, "warmup": WARMUP, "batch_operations": BATCH, "minimum_sample_time_ns": SAMPLE_TIME.as_nanos() as u64, "allocation_samples": 1, "clock": "per_operation"});
        if let Some(identity) = &self.identity {
            if self.diagnostic {
                protocol["sampling_control"] = json!({
                    "mode": if self.fixed { "fixed" } else { "timed" },
                    "iterations_sha256": identity,
                });
            } else {
                protocol["fixed_iterations_sha256"] = json!(identity);
            }
        }
        protocol
    }
}

#[derive(Serialize)]
struct Allocation {
    count_total: u64,
    count_peak: u64,
    count_retained: i64,
    bytes_total: u64,
    bytes_peak: u64,
    bytes_retained: i64,
}

impl From<AllocationInfo> for Allocation {
    fn from(info: AllocationInfo) -> Self {
        Self {
            count_total: info.count_total,
            count_peak: info.count_max,
            count_retained: info.count_current,
            bytes_total: info.bytes_total,
            bytes_peak: info.bytes_max,
            bytes_retained: info.bytes_current,
        }
    }
}

#[derive(Serialize)]
struct Timing {
    elapsed_ns: [u64; SAMPLES],
    iterations: [usize; SAMPLES],
    median_ns: f64,
}

impl Timing {
    fn new(elapsed_ns: [u64; SAMPLES], iterations: [usize; SAMPLES]) -> Self {
        let mut sorted: [f64; SAMPLES] =
            std::array::from_fn(|index| elapsed_ns[index] as f64 / iterations[index] as f64);
        sorted.sort_by(f64::total_cmp);
        Self {
            elapsed_ns,
            iterations,
            median_ns: sorted[SAMPLES / 2],
        }
    }
}

#[derive(Serialize)]
struct Measurement {
    name: String,
    input_bytes: usize,
    input_utf16: usize,
    input_sha256: String,
    output_sha256: String,
    tokens: usize,
    complete_polls: usize,
    cancel_at_poll: usize,
    verified_cancellations: usize,
    verified_recoveries: usize,
    remaining_budget_bytes: usize,
    sample_wall_ns: [u64; SAMPLES],
    allocation: Allocation,
    operation_timing: Timing,
    response_timing: Timing,
}

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

struct Samples {
    operation: Timing,
    response: Timing,
    wall_ns: [u64; SAMPLES],
}

fn sample(
    runner: &Runner<'_>,
    text: &str,
    budget: &MemoryBudget,
    at: usize,
    expected: &NoriOutput,
    limit: Option<usize>,
) -> Samples {
    let mut operation = [0; SAMPLES];
    let mut response = [0; SAMPLES];
    let mut iterations = [0; SAMPLES];
    let mut sample_wall_ns = [0; SAMPLES];
    opt_out(|| {
        for _ in 0..WARMUP {
            black_box(cancel(runner, text, budget, at));
            recover(runner, text, budget, expected);
        }
        for sample in 0..SAMPLES {
            let mut total = 0;
            let mut propagation = 0;
            let mut count = 0;
            let started = Instant::now();
            loop {
                for _ in 0..BATCH {
                    let (elapsed, returned) = cancel(runner, text, budget, at);
                    total += elapsed;
                    propagation += returned;
                }
                count += BATCH;
                if limit.map_or_else(|| started.elapsed() >= SAMPLE_TIME, |limit| count >= limit) {
                    break;
                }
            }
            sample_wall_ns[sample] = started.elapsed().as_nanos() as u64;
            assert!(
                u128::from(sample_wall_ns[sample]) >= SAMPLE_TIME.as_nanos(),
                "sampling control is too short"
            );
            iterations[sample] = count;
            operation[sample] = total;
            response[sample] = propagation;
            recover(runner, text, budget, expected);
        }
    });
    Samples {
        operation: Timing::new(operation, iterations),
        response: Timing::new(response, iterations),
        wall_ns: sample_wall_ns,
    }
}

fn cases(
    case: &Case,
    stage: &str,
    mode: DecompoundMode,
    runner: &Runner<'_>,
    sampling: &Sampling,
) -> Vec<Measurement> {
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
    let mut results = Vec::with_capacity(3);
    for (point, at) in [
        ("first", 1),
        ("middle", complete_polls.div_ceil(2)),
        ("last", complete_polls),
    ] {
        let limit = sampling.limit(stage, mode, case, point);
        let samples = sample(runner, &case.text, &budget, at, &expected, limit);
        let info = measure(|| {
            black_box(cancel(runner, &case.text, &budget, at));
        });
        assert_eq!(info.bytes_current, 0);
        assert_eq!(info.count_current, 0);
        recover(runner, &case.text, &budget, &expected);
        results.push(Measurement {
            name: format!("{stage}/{mode:?}/{}/{point}", case.name),
            input_bytes: case.text.len(),
            input_utf16: case.text.encode_utf16().count(),
            input_sha256: format!("{:x}", Sha256::digest(case.text.as_bytes())),
            output_sha256: output_sha256.clone(),
            tokens: expected.tokens.len(),
            complete_polls,
            cancel_at_poll: at,
            verified_cancellations: WARMUP + samples.operation.iterations.iter().sum::<usize>() + 1,
            verified_recoveries: WARMUP + SAMPLES + 1,
            remaining_budget_bytes: budget.used(),
            allocation: info.into(),
            sample_wall_ns: samples.wall_ns,
            operation_timing: samples.operation,
            response_timing: samples.response,
        });
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
    let sampling = Sampling::load();
    let resources = NoriResources::default();
    let resolved = resources.load_default().expect("bundled dictionary");
    let corpus: Corpus = serde_json::from_str(CORPUS).expect("fixed corpus");
    // Timing-dependent JSON number strings must not change subsequent heap state.
    let mut results = Vec::with_capacity(corpus.cases.len() * 3 * 2 * 3);
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
                &sampling,
            ));
            results.extend(cases(
                &case,
                "analyzer",
                mode,
                &Runner::Analyzer(&analyzer),
                &sampling,
            ));
        }
    }
    let fixed = sampling.fixed && !sampling.diagnostic;
    let mut report = json!({
        "schema_version": if fixed { 3 } else { 2 },
        "owner": "uqa-analysis",
        "purpose": "cooperative_cancellation",
        "target_arch": std::env::consts::ARCH,
        "target_os": std::env::consts::OS,
        "pointer_bits": usize::BITS,
        "threads": 1,
        "protocol": sampling.protocol(),
        "memory_limit_bytes": ALLOWANCE,
        "unrelated_reservation_bytes": RETAINED,
        "bundle_bytes": uqa_nori_data::BUNDLE.len(),
        "bundle_sha256": uqa_nori_data::BUNDLE_SHA256,
        "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "timing_scope": "operation: entry to cancelled return; response: callback decision to cancelled return, including workspace destruction and clock-call overhead; excludes verification and subsequent recovery; no external-signal detection or scheduler latency claim",
        "allocation_scope": "current-thread Rust System allocator requests during one cancelled operation; excludes preloaded dictionary, input/reference output, allowance handle, unrelated reservation, and host memory",
        "measurements": results,
    });
    if fixed {
        report["sampling_iterations"] = json!(sampling.iterations);
    }
    report
}
