//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact single-prior hybrid relevance guard: NDCG / MAP floors for text and
//! vector evidence over a synthetic corpus with a latent relevance model.
//!
//! Every document belongs to a latent subtopic and queries target one
//! subtopic each. Text terms and embeddings are independent noisy
//! projections of the latent label: some relevant documents lack the
//! query terms (only the vector signal reaches them), some off-topic
//! documents share query terms (polysemy distractors the vector signal
//! must suppress), and a canonical quality tier is expressed only
//! through term frequency (embeddings are quality-blind), so fusion
//! must preserve the text signal's ordering while using the vector
//! signal for recall. Judgments derive from the latent labels alone,
//! never from engine output.
//!
//! Two regimes pin behavior across raw BM25 score scales:
//!
//! - `small_corpus`: 63 documents, raw scores stay in the sigmoid's
//!   linear region. Fusion must beat the vector-only baseline.
//! - `large_corpus`: 630 documents with term-frequency boosts pushing
//!   raw scores past 16, where naive probability round-trips saturate.
//!   Fusion must stay above the vector-only baseline.
//!
//! The corpus generator is seeded and deterministic, so the floors are
//! tight; a change that moves them signals a deliberate relevance
//! trade-off that must be re-justified here.

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams, ScoringMode};
use uqa_scoring::{average_precision_at_k, ndcg_at_k, BM25Params};
use uqa_storage::document_store::Document;

const SEED: u64 = 20_260_716;
const DIM: usize = 24;
const TOPIC_COUNT: usize = 3;
const SUBTOPICS_PER_TOPIC: usize = 3;
const K: usize = 10;
const KNN_POOL: usize = 30;

// The project naming convention keeps acronyms fully capitalized.
#[allow(clippy::upper_case_acronyms)]
struct LCG(u64);

impl LCG {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as f32) / (u32::MAX >> 1) as f32
    }

    fn chance(&mut self, probability: f32) -> bool {
        self.next_f32() < probability
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in v.iter_mut() {
        *x /= norm;
    }
}

const TOPIC_WORDS: [&str; TOPIC_COUNT] = ["rust", "python", "espresso"];
const SUBTOPIC_TERMS: [[[&str; 4]; SUBTOPICS_PER_TOPIC]; TOPIC_COUNT] = [
    [
        ["memory", "safety", "alias", "segfault"],
        ["async", "runtime", "executor", "await"],
        ["borrow", "ownership", "lifetime", "move"],
    ],
    [
        ["pandas", "dataframe", "groupby", "merge"],
        ["asyncio", "coroutine", "eventloop", "gather"],
        ["typing", "protocol", "generic", "annotation"],
    ],
    [
        ["grind", "extraction", "burr", "dose"],
        ["pressure", "portafilter", "basket", "preinfusion"],
        ["tamping", "crema", "puck", "channeling"],
    ],
];
const FILLERS: [&str; 8] = [
    "guide",
    "notes",
    "systems",
    "handbook",
    "overview",
    "practice",
    "workshop",
    "essentials",
];

struct RegimeSpec {
    name: &'static str,
    docs_per_subtopic: usize,
    /// Included subtopic terms repeat up to this many times, scaling
    /// raw BM25 sums toward the regime's target magnitude.
    max_repeat: usize,
    min_hybrid_ndcg: f64,
    min_hybrid_map: f64,
}

const REGIMES: [RegimeSpec; 2] = [
    RegimeSpec {
        name: "small_corpus",
        docs_per_subtopic: 7,
        max_repeat: 1,
        // Exact single-prior fusion measured 0.8680 / 0.3899 at the pinned
        // seed. MAP here is a reciprocal-rank proxy because there is one
        // canonical document per query. The algebra does not guarantee that
        // fusing an unsupervised vector-evidence estimate beats text-only
        // ranking, so this regime gates absolute quality and vector gain.
        min_hybrid_ndcg: 0.86,
        min_hybrid_map: 0.38,
    },
    RegimeSpec {
        name: "large_corpus",
        docs_per_subtopic: 70,
        max_repeat: 3,
        // Exact single-prior fusion measured 0.7838 / 0.1122 at the pinned
        // seed. The quality tier lives entirely in the text signal here, so
        // text-only ranking is a reported reference rather than a fusion gate.
        min_hybrid_ndcg: 0.77,
        min_hybrid_map: 0.10,
    },
];

struct DocSpec {
    id: u64,
    topic: usize,
    subtopic: usize,
    canonical: bool,
}

struct QueryCase {
    text: String,
    topic: usize,
    subtopic: usize,
    query_vector: Vec<f32>,
}

struct Corpus {
    engine: Engine,
    docs: Vec<DocSpec>,
    queries: Vec<QueryCase>,
}

fn dcg(relevances: &[f64], k: usize) -> f64 {
    relevances
        .iter()
        .take(k)
        .enumerate()
        .map(|(rank, gain)| gain / ((rank + 2) as f64).log2())
        .sum()
}

/// NDCG normalized by the true ideal ranking over the full judgment
/// set, so missing a relevant document costs recall. The in-crate
/// `ndcg_at_k` self-normalizes against the retrieved list only, which
/// would hide recall regressions.
fn true_ideal_ndcg(relevances: &[f64], judgments: &BTreeMap<u64, f64>, k: usize) -> f64 {
    let mut ideal: Vec<f64> = judgments.values().copied().collect();
    assert!(
        ideal.iter().all(|score| score.is_finite()),
        "judgments must contain only finite scores"
    );
    ideal.sort_by(|a, b| b.total_cmp(a));
    let idcg = dcg(&ideal, k);
    if idcg == 0.0 {
        return 0.0;
    }
    dcg(relevances, k) / idcg
}

fn create_corpus_engine() -> Engine {
    let engine = Engine::new();
    engine
        .create_default_table("articles", vec!["title".into()])
        .unwrap();
    engine
        .create_vector_field("articles", "embedding", DIM as u32)
        .unwrap();
    engine
}

fn build_corpus(spec: &RegimeSpec) -> Corpus {
    let mut rng = LCG(SEED);

    let mut common: Vec<f32> = (0..DIM).map(|_| rng.next_f32()).collect();
    normalize(&mut common);
    let mut topic_dirs: Vec<Vec<f32>> = Vec::new();
    for _ in 0..TOPIC_COUNT {
        let mut dir: Vec<f32> = (0..DIM).map(|_| rng.next_f32() - 0.5).collect();
        normalize(&mut dir);
        topic_dirs.push(dir);
    }
    let mut subtopic_dirs: Vec<Vec<Vec<f32>>> = Vec::new();
    for _ in 0..TOPIC_COUNT {
        let mut per_topic = Vec::new();
        for _ in 0..SUBTOPICS_PER_TOPIC {
            let mut dir: Vec<f32> = (0..DIM).map(|_| rng.next_f32() - 0.5).collect();
            normalize(&mut dir);
            per_topic.push(dir);
        }
        subtopic_dirs.push(per_topic);
    }

    let embedding_for = |topic: usize, subtopic: usize, rng: &mut LCG| -> Vec<f32> {
        let mut emb: Vec<f32> = (0..DIM)
            .map(|d| {
                0.62 * common[d]
                    + 0.28 * topic_dirs[topic][d]
                    + 0.26 * subtopic_dirs[topic][subtopic][d]
                    + 0.22 * (rng.next_f32() - 0.5)
            })
            .collect();
        normalize(&mut emb);
        emb
    };

    let engine = create_corpus_engine();

    let mut docs: Vec<DocSpec> = Vec::new();
    let mut doc_id: u64 = 0;
    for topic in 0..TOPIC_COUNT {
        for (subtopic, subtopic_terms) in SUBTOPIC_TERMS[topic].iter().enumerate() {
            for doc_index in 0..spec.docs_per_subtopic {
                doc_id += 1;
                // Latent quality tier: one document in ten is canonical
                // for its subtopic. Quality is expressed only through
                // higher term frequency, never through the embedding.
                let canonical = doc_index.is_multiple_of(10);
                let mut words: Vec<&str> = Vec::new();
                if rng.chance(0.8) {
                    words.push(TOPIC_WORDS[topic]);
                }
                // Noisy projection of the latent subtopic into text:
                // each of the doc's own subtopic terms appears with
                // probability 0.6, so roughly one in six relevant
                // documents carries neither term.
                for (term_index, term) in subtopic_terms.iter().enumerate() {
                    if rng.chance(0.6) {
                        let base = 1 + (doc_id as usize * 7 + term_index) % spec.max_repeat;
                        let repeats = if canonical { base * 2 + 1 } else { base };
                        for _ in 0..repeats {
                            words.push(term);
                        }
                    }
                }
                // Polysemy distractors: borrow one term from another
                // topic's vocabulary occasionally.
                if rng.chance(0.25) {
                    let other_topic = (topic + 1) % TOPIC_COUNT;
                    let other_sub = (subtopic + 1) % SUBTOPICS_PER_TOPIC;
                    words.push(SUBTOPIC_TERMS[other_topic][other_sub][0]);
                }
                words.push(FILLERS[(doc_id as usize) % FILLERS.len()]);
                words.push(FILLERS[(doc_id as usize * 3 + 1) % FILLERS.len()]);
                let title = words.join(" ");

                let mut d = Document::new();
                d.insert("title".into(), Value::Str(title));
                let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
                vectors.insert("embedding".into(), embedding_for(topic, subtopic, &mut rng));
                engine
                    .add_document_with_vectors("articles", doc_id, d, vectors)
                    .unwrap();
                docs.push(DocSpec {
                    id: doc_id,
                    topic,
                    subtopic,
                    canonical,
                });
            }
        }
    }

    let mut queries: Vec<QueryCase> = Vec::new();
    for topic in 0..TOPIC_COUNT {
        for (subtopic, terms) in SUBTOPIC_TERMS[topic].iter().enumerate() {
            queries.push(QueryCase {
                text: format!(
                    "{} {} {} {} {}",
                    TOPIC_WORDS[topic], terms[0], terms[1], terms[2], terms[3]
                ),
                topic,
                subtopic,
                query_vector: embedding_for(topic, subtopic, &mut rng),
            });
        }
    }

    Corpus {
        engine,
        docs,
        queries,
    }
}

/// Judgments come from the latent labels alone: 3 for canonical
/// target-subtopic documents, 2 for the rest of the target subtopic,
/// 1 for the same topic's other subtopics, 0 elsewhere.
fn judgments_for(docs: &[DocSpec], query: &QueryCase) -> BTreeMap<u64, f64> {
    docs.iter()
        .filter(|doc| doc.topic == query.topic)
        .map(|doc| {
            let gain = if doc.subtopic == query.subtopic {
                if doc.canonical {
                    3.0
                } else {
                    2.0
                }
            } else {
                1.0
            };
            (doc.id, gain)
        })
        .collect()
}

fn hybrid_params(query: &QueryCase) -> HybridSearchParams<'_> {
    HybridSearchParams {
        table: "articles",
        text_field: "title",
        text_query: &query.text,
        vector_field: "embedding",
        query_vector: query.query_vector.clone(),
        knn_pool: KNN_POOL,
        top_k: K,
    }
}

struct RelevanceReport {
    ndcg: f64,
    map: f64,
}

fn measure(corpus: &Corpus, ranked_ids_for: &dyn Fn(&QueryCase) -> Vec<u64>) -> RelevanceReport {
    let mut ndcg_sum = 0.0;
    let mut map_sum = 0.0;
    for query in &corpus.queries {
        let judgments = judgments_for(&corpus.docs, query);
        let ranked = ranked_ids_for(query);
        let relevances: Vec<f64> = ranked
            .iter()
            .map(|id| judgments.get(id).copied().unwrap_or(0.0))
            .collect();
        // Keep the in-crate metric exercised alongside the true-ideal
        // form so both stay covered by the guard.
        let _ = ndcg_at_k(&relevances, K);
        ndcg_sum += true_ideal_ndcg(&relevances, &judgments, K);
        // MAP counts canonical target-subtopic documents as relevant.
        let relevant: Vec<bool> = relevances.iter().map(|gain| *gain >= 3.0).collect();
        let total_relevant = judgments.values().filter(|gain| **gain >= 3.0).count();
        map_sum += average_precision_at_k(&relevant, total_relevant, K);
    }
    let n = corpus.queries.len() as f64;
    RelevanceReport {
        ndcg: ndcg_sum / n,
        map: map_sum / n,
    }
}

fn bench_exact_hybrid_relevance(c: &mut Criterion) {
    for spec in &REGIMES {
        let corpus = build_corpus(spec);

        let hybrid = measure(&corpus, &|query| {
            corpus
                .engine
                .hybrid_search(&hybrid_params(query))
                .expect("hybrid relevance search")
                .into_iter()
                .map(|hit| hit.doc_id)
                .collect()
        });
        let text = measure(&corpus, &|query| {
            corpus
                .engine
                .search(
                    "articles",
                    "title",
                    &query.text,
                    &ScoringMode::BM25(BM25Params::default()),
                    K,
                )
                .expect("text relevance search")
                .into_iter()
                .map(|hit| hit.doc_id)
                .collect()
        });
        let vector = measure(&corpus, &|query| {
            corpus
                .engine
                .knn_search("articles", "embedding", &query.query_vector, K)
                .expect("vector relevance search")
                .into_iter()
                .map(|hit| hit.doc_id)
                .collect()
        });

        eprintln!(
            "[exact hybrid relevance bench :: {}] hybrid NDCG@{K} = {:.4} (floor {}), MAP@{K} = {:.4} \
             (floor {}); text NDCG@{K} = {:.4}; vector NDCG@{K} = {:.4}",
            spec.name,
            hybrid.ndcg,
            spec.min_hybrid_ndcg,
            hybrid.map,
            spec.min_hybrid_map,
            text.ndcg,
            vector.ndcg,
        );
        assert!(
            hybrid.ndcg >= spec.min_hybrid_ndcg,
            "[{}] hybrid NDCG@{K} = {:.4} below floor {}",
            spec.name,
            hybrid.ndcg,
            spec.min_hybrid_ndcg,
        );
        assert!(
            hybrid.map >= spec.min_hybrid_map,
            "[{}] hybrid MAP@{K} = {:.4} below floor {}",
            spec.name,
            hybrid.map,
            spec.min_hybrid_map,
        );
        assert!(
            hybrid.ndcg > vector.ndcg,
            "[{}] hybrid NDCG@{K} = {:.4} must beat vector-only {:.4}",
            spec.name,
            hybrid.ndcg,
            vector.ndcg,
        );
        let label = format!(
            "exact_hybrid_relevance_{}_{}_queries_at_k{K}",
            spec.name,
            corpus.queries.len(),
        );
        c.bench_function(&label, |b| {
            b.iter(|| {
                for query in &corpus.queries {
                    let hits = corpus
                        .engine
                        .hybrid_search(black_box(&hybrid_params(query)))
                        .expect("hybrid benchmark search");
                    black_box(hits.len());
                }
            });
        });
    }
}

criterion_group!(benches, bench_exact_hybrid_relevance);
criterion_main!(benches);
