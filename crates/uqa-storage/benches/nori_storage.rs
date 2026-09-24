//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native graph indexing and incremental batch allocation measurements.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use allocation_counter::{measure, opt_out, AllocationInfo};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::{nori::nori_analyzer, whitespace_analyzer, CompiledAnalyzer};
use uqa_storage::{
    clustered_postings::encode_occurrence_cluster, AnalyzerPhase, InvertedIndex,
    MemoryInvertedIndex,
};

const CORPUS: &str = include_str!("../../uqa-analysis/benches/nori/corpus.json");
const SAMPLES: usize = 7;
const APPEND_DOCUMENTS: u64 = 16;

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    text: String,
    repeat: usize,
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn fresh(revision: &Arc<CompiledAnalyzer>) -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(whitespace_analyzer());
    index
        .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
        .unwrap();
    index
}

fn add_points(index: &mut MemoryInvertedIndex, count: u64, texts: &[String]) {
    for id in 0..count {
        index
            .add_document(id, fields(&texts[id as usize % texts.len()]))
            .unwrap();
    }
}

fn append_batch(index: &mut MemoryInvertedIndex, start: u64, count: u64, texts: &[String]) {
    let documents = (start..start + count)
        .map(|id| (id, fields(&texts[id as usize % texts.len()])))
        .collect();
    index.try_add_documents(documents).unwrap();
}

fn allocation(info: AllocationInfo) -> Value {
    json!({
        "count_total": info.count_total, "count_peak": info.count_max,
        "count_net": info.count_current,
        "bytes_total": info.bytes_total, "bytes_peak": info.bytes_max,
        "bytes_net": info.bytes_current,
    })
}

fn append_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn graph_fingerprint(index: &MemoryInvertedIndex, documents: u64) -> String {
    assert_eq!(index.doc_count().unwrap(), documents);
    let mut hash = Sha256::new();
    hash.update(documents.to_le_bytes());
    for id in 0..documents {
        hash.update(id.to_le_bytes());
        append_bytes(
            &mut hash,
            &index
                .indexed_field_metadata(id, "body")
                .unwrap()
                .unwrap()
                .to_bytes()
                .unwrap(),
        );
    }
    for key in index.vocabulary_keys("body").unwrap() {
        append_bytes(&mut hash, key.as_bytes());
        let postings = index.get_occurrence_postings("body", &key).unwrap();
        let (scores, occurrences) = encode_occurrence_cluster(&postings).unwrap();
        append_bytes(&mut hash, &scores);
        append_bytes(&mut hash, &occurrences);
    }
    format!("{:x}", hash.finalize())
}

fn probe(
    name: &str,
    documents: u64,
    allocation_only: bool,
    setup: impl Fn() -> MemoryInvertedIndex,
    mutate: impl Fn(&mut MemoryInvertedIndex),
) -> Value {
    let mut elapsed_ns = Vec::new();
    if !allocation_only {
        elapsed_ns.reserve(SAMPLES);
        opt_out(|| {
            let mut warmup = setup();
            mutate(&mut warmup);
            drop(warmup);
            for _ in 0..SAMPLES {
                let mut index = setup();
                let start = Instant::now();
                mutate(black_box(&mut index));
                elapsed_ns.push(start.elapsed().as_nanos() as u64);
                drop(index);
            }
        });
    }
    let mut index = setup();
    let info = measure(|| mutate(&mut index));
    let fingerprint = graph_fingerprint(&index, documents);
    eprintln!("measured {name}");
    let mut row = json!({
        "name": name, "documents_after": documents,
        "allocation": allocation(info), "graph_sha256": fingerprint,
        "field_length": index.total_field_length("body").unwrap(),
        "posting_count": index.posting_count(Some("body")).unwrap(),
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
    let corpus: Corpus = serde_json::from_str(CORPUS).unwrap();
    let texts: Vec<_> = corpus
        .cases
        .into_iter()
        .map(|case| case.text.repeat(case.repeat))
        .collect();
    let revision = nori_analyzer().compile().unwrap();
    let mut measurements = Vec::new();
    for count in [256_u64, 2_048] {
        measurements.push(probe(
            &format!("build_points/{count}"),
            count,
            allocation_only,
            || fresh(&revision),
            |index| add_points(index, count, &texts),
        ));
    }
    for count in [0_u64, 256, 2_048] {
        let mut seed = fresh(&revision);
        add_points(&mut seed, count, &texts);
        measurements.push(probe(
            &format!("append_batch_16/{count}"),
            count + APPEND_DOCUMENTS,
            allocation_only,
            || seed.clone(),
            |index| append_batch(index, count, APPEND_DOCUMENTS, &texts),
        ));
    }
    let mut report = json!({
        "schema_version": 1, "owner": "uqa-storage", "target_arch": std::env::consts::ARCH,
        "target_os": std::env::consts::OS, "pointer_bits": usize::BITS, "threads": 1,
        "allocation_scope": "current-thread Rust allocator requests during mutation; net includes released seed allocations; excludes static bundle, base index, stack and host heap",
        "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "analyzer_fingerprint": revision.descriptor().fingerprint().to_string(),
        "measurements": measurements,
    });
    report["protocol"] = if allocation_only {
        json!({"allocation_samples": 1, "samples": 0, "warmup": 0, "timed_operations_per_sample": 0})
    } else {
        report["timing_scope"] = json!("input construction and index mutation; seed cloning, destruction and graph validation excluded");
        json!({"samples": SAMPLES, "warmup": 1, "timed_operations_per_sample": 1})
    };
    println!("{report}");
}
