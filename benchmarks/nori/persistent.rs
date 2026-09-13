//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared measurement protocol for provider-owned benchmark entrypoints.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use allocation_counter::{measure, opt_out};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::{nori::nori_analyzer, whitespace_analyzer, CompiledAnalyzer};
use uqa_storage::{
    clustered_postings::encode_occurrence_cluster, AnalyzerPhase, InvertedIndex,
    MemoryInvertedIndex,
};

const CORPUS: &str = include_str!("../../crates/uqa-analysis/benches/nori/corpus.json");
const SAMPLES: usize = 7;

pub trait Session {
    type Index: InvertedIndex;
    fn open(path: &Path) -> Self;
    fn index(&mut self) -> &mut Self::Index;
    fn begin(&self);
    fn finish(&self, rollback: bool);
}

fn bind(index: &mut impl InvertedIndex, revision: &Arc<CompiledAnalyzer>) {
    index
        .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
        .unwrap();
}

fn append(index: &mut impl InvertedIndex, start: u64, count: u64, texts: &[String]) {
    let documents = (start..start + count)
        .map(|id| {
            (
                id,
                BTreeMap::from([("body".into(), texts[id as usize % texts.len()].clone())]),
            )
        })
        .collect();
    index.try_add_documents(documents).unwrap();
}

fn append_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn graph(index: &impl InvertedIndex, documents: u64) -> Value {
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
    json!({
        "graph_sha256": format!("{:x}", hash.finalize()),
        "field_length": index.total_field_length("body").unwrap(),
        "posting_count": index.posting_count(Some("body")).unwrap(),
    })
}

fn seed<S: Session>(path: &Path, count: u64, texts: &[String], revision: &Arc<CompiledAnalyzer>) {
    let mut session = S::open(path);
    bind(session.index(), revision);
    session.begin();
    append(session.index(), 0, count, texts);
    session.finish(false);
    // Drop every database handle before cloning the quiescent seed files.
}

fn copy_seed(seed: &Path, target: &Path) {
    for file in std::fs::read_dir(seed).unwrap() {
        let file = file.unwrap();
        assert!(file.file_type().unwrap().is_file());
        std::fs::copy(file.path(), target.join(file.file_name())).unwrap();
    }
}

fn file_bytes(directory: &Path) -> u64 {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

fn probe<S: Session>(
    name: &str,
    base: u64,
    added: u64,
    rollback: bool,
    texts: &[String],
    revision: &Arc<CompiledAnalyzer>,
) -> Value {
    let documents = base + if rollback { 0 } else { added };
    let seed_dir = tempfile::tempdir().unwrap();
    seed::<S>(&seed_dir.path().join("index.db"), base, texts, revision);
    let mut reference = MemoryInvertedIndex::new(whitespace_analyzer());
    bind(&mut reference, revision);
    append(&mut reference, 0, documents, texts);
    let expected = graph(&reference, documents);
    drop(reference);

    let setup = || {
        let directory = tempfile::tempdir().unwrap();
        copy_seed(seed_dir.path(), directory.path());
        let mut session = S::open(&directory.path().join("index.db"));
        bind(session.index(), revision);
        (session, directory)
    };
    let mutate = |session: &mut S| {
        session.begin();
        append(session.index(), base, added, texts);
        session.finish(rollback);
    };
    let verify = |mut session: S, directory: &Path| {
        let live = graph(session.index(), documents);
        assert_eq!(live, expected, "live provider graph differs from Memory");
        drop(session);
        let mut reopened = S::open(&directory.join("index.db"));
        bind(reopened.index(), revision);
        assert_eq!(
            graph(reopened.index(), documents),
            expected,
            "reopened provider graph differs from Memory"
        );
        drop(reopened);
    };
    let mut elapsed_ns = Vec::with_capacity(SAMPLES);
    opt_out(|| {
        for sample in 0..=SAMPLES {
            let (mut session, directory) = setup();
            let start = Instant::now();
            mutate(black_box(&mut session));
            let elapsed = start.elapsed().as_nanos() as u64;
            if sample > 0 {
                elapsed_ns.push(elapsed);
            }
            verify(session, directory.path());
        }
    });
    let (mut session, directory) = setup();
    let info = measure(|| mutate(&mut session));
    verify(session, directory.path());
    let mut ordered = elapsed_ns.clone();
    ordered.sort_unstable();
    eprintln!("measured {name}");
    json!({
        "name": name, "documents_after": documents,
        "elapsed_ns": elapsed_ns, "median_ns": ordered[SAMPLES / 2],
        "allocation": {
            "count_total": info.count_total, "count_peak": info.count_max, "count_net": info.count_current,
            "bytes_total": info.bytes_total, "bytes_peak": info.bytes_max, "bytes_net": info.bytes_current,
        },
        "graph_sha256": expected["graph_sha256"], "field_length": expected["field_length"],
        "posting_count": expected["posting_count"], "verified_live_and_reopened_samples": SAMPLES + 2,
        "closed_seed_file_bytes": file_bytes(seed_dir.path()),
        "closed_result_file_bytes": file_bytes(directory.path()),
    })
}

pub fn run<S: Session>(owner: &str, durability: &str) {
    let corpus: Value = serde_json::from_str(CORPUS).unwrap();
    let texts: Vec<_> = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            case["text"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap() as usize)
        })
        .collect();
    let revision = nori_analyzer().compile().unwrap();
    let mut measurements = Vec::new();
    for (name, base, added, rollback) in [
        ("commit_batch_256/0", 0, 256, false),
        ("commit_batch_16/256", 256, 16, false),
        ("commit_batch_16/2048", 2048, 16, false),
        ("rollback_batch_16/2048", 2048, 16, true),
    ] {
        measurements.push(probe::<S>(name, base, added, rollback, &texts, &revision));
    }
    println!(
        "{}",
        json!({
            "schema_version": 1, "owner": owner, "target_arch": std::env::consts::ARCH,
            "target_os": std::env::consts::OS, "pointer_bits": usize::BITS, "threads": 1,
            "protocol": {"samples": SAMPLES, "warmup": 1, "timed_operations_per_sample": 1},
            "timing_scope": "input construction, index mutation and explicit transaction begin/commit/rollback; excludes seed copying, open, close, graph verification and reopen",
            "allocation_scope": "current-thread Rust allocator requests during mutation and transaction; excludes base index, dictionary, C allocator, OS cache, stack and host heap",
            "filesystem": if cfg!(target_os = "emscripten") { "Emscripten virtual filesystem; no host durability measurement" } else { "temporary directory on host filesystem" },
            "durability": durability,
            "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
            "analyzer_fingerprint": revision.descriptor().fingerprint().to_string(),
            "measurements": measurements,
        })
    );
}
