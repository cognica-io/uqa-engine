//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned BEIR manifest and prepared-artifact loading.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use uqa_core::Value;
use uqa_sql::SQLParam;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct DatasetSpec {
    pub name: String,
    pub url: String,
    pub archive_sha256: String,
    pub qrels_split: String,
    pub expected_corpus_count: usize,
    pub expected_query_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct EmbeddingSpec {
    pub provider: String,
    pub package_version: String,
    pub model: String,
    pub revision: String,
    pub dimensions: usize,
    pub normalize_embeddings: bool,
    pub device: String,
    pub batch_size: usize,
    pub max_sequence_length: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ExecutionSpec {
    pub api: String,
    pub load: String,
    pub index_lifecycle: String,
    pub text_query: String,
    pub vector_query: String,
    pub hybrid_query: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct IndexSpec {
    pub name: String,
    pub kind: String,
    pub statement: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct WorkloadSpec {
    pub top_k: usize,
    pub knn_pool: usize,
    pub quality_query_count: usize,
    pub performance_query_count: usize,
    pub insert_batch_rows: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct SystemSpec {
    pub name: String,
    pub query: String,
    pub score_domain: String,
    pub require_top_k: bool,
    pub criterion_benchmark: String,
    pub minimum_quality: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct MeasurementSpec {
    pub criterion_point_estimator: String,
    pub sample_size: usize,
    pub warm_up_seconds: u64,
    pub measurement_seconds: u64,
    pub query_throughput_unit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct BenchmarkManifest {
    pub schema_version: u64,
    pub description: String,
    pub dataset: DatasetSpec,
    pub embedding: EmbeddingSpec,
    pub storage: JsonValue,
    pub execution: ExecutionSpec,
    pub indexes: Vec<IndexSpec>,
    pub workload: WorkloadSpec,
    pub systems: Vec<SystemSpec>,
    pub comparative_quality: JsonValue,
    pub measurement: MeasurementSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ArtifactSpec {
    pub path: String,
    pub rows: usize,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
struct PreparedManifest {
    schema_version: u64,
    benchmark_manifest_sha256_at_preparation: String,
    dataset: DatasetSpec,
    embedding: EmbeddingSpec,
    preparation: JsonValue,
    artifacts: BTreeMap<String, ArtifactSpec>,
}

#[derive(Debug, Deserialize)]
struct RawDocument {
    id: u64,
    source_id: String,
    body: String,
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    id: String,
    text: String,
    judgments: BTreeMap<String, f64>,
    embedding: Vec<f32>,
}

pub(super) struct DocumentCase {
    pub id: u64,
    pub source_id: String,
    pub body: String,
    pub embedding: Vec<f32>,
}

pub(super) struct QueryCase {
    pub id: String,
    pub judgments: BTreeMap<u64, f64>,
    pub params: [SQLParam; 2],
}

pub(super) struct Fixture {
    pub manifest: BenchmarkManifest,
    pub manifest_sha256: String,
    pub preparation: JsonValue,
    pub artifacts: BTreeMap<String, ArtifactSpec>,
    pub documents: Vec<DocumentCase>,
    pub queries: Vec<QueryCase>,
}

impl BenchmarkManifest {
    pub fn query_sql(&self, system: &SystemSpec) -> &str {
        match system.query.as_str() {
            "text_query" => &self.execution.text_query,
            "vector_query" => &self.execution.vector_query,
            "hybrid_query" => &self.execution.hybrid_query,
            other => panic!("unknown BEIR system query identity {other:?}"),
        }
    }

    fn validate(&self) {
        assert_eq!(self.schema_version, 1, "unsupported BEIR manifest schema");
        assert_eq!(self.storage["backend"], "sqlite");
        assert_eq!(self.storage["persistent"], true);
        assert_eq!(self.execution.api, "Engine::sql");
        assert!(self.embedding.normalize_embeddings);
        assert!(self.embedding.dimensions > 0);
        assert_eq!(
            self.workload.quality_query_count,
            self.dataset.expected_query_count
        );
        assert!(self.workload.top_k > 0);
        assert!(self.workload.knn_pool >= self.workload.top_k);
        assert!(self.workload.performance_query_count <= self.workload.quality_query_count);
        assert!(self.workload.insert_batch_rows > 0);
        assert!(self.measurement.sample_size >= 10);
        assert_eq!(self.measurement.criterion_point_estimator, "mean");
        assert_eq!(self.systems.len(), 3);
        let names: BTreeSet<_> = self.systems.iter().map(|system| &system.name).collect();
        assert_eq!(names.len(), self.systems.len());
        for system in &self.systems {
            let _ = self.query_sql(system);
        }
    }
}

impl Fixture {
    pub fn load(manifest_path: &Path, data_directory: &Path) -> Self {
        let manifest_bytes = fs::read(manifest_path).expect("read BEIR benchmark manifest");
        let manifest: BenchmarkManifest =
            serde_json::from_slice(&manifest_bytes).expect("decode BEIR benchmark manifest");
        manifest.validate();
        let manifest_sha256 = sha256_bytes(&manifest_bytes);
        let prepared_path = data_directory.join("prepared-manifest.json");
        let prepared: PreparedManifest =
            serde_json::from_slice(&fs::read(&prepared_path).expect("read prepared BEIR manifest"))
                .expect("decode prepared BEIR manifest");
        assert_eq!(prepared.schema_version, 1);
        assert_eq!(prepared.dataset, manifest.dataset);
        assert_eq!(prepared.embedding, manifest.embedding);
        assert_eq!(prepared.benchmark_manifest_sha256_at_preparation.len(), 64);
        let corpus_artifact = artifact(&prepared.artifacts, "corpus", data_directory);
        let query_artifact = artifact(&prepared.artifacts, "queries", data_directory);
        let documents = read_documents(&corpus_artifact, manifest.embedding.dimensions);
        let queries = read_queries(
            &query_artifact,
            manifest.embedding.dimensions,
            documents.len(),
        );
        assert_eq!(documents.len(), manifest.dataset.expected_corpus_count);
        assert_eq!(queries.len(), manifest.dataset.expected_query_count);
        Self {
            manifest,
            manifest_sha256,
            preparation: prepared.preparation,
            artifacts: prepared.artifacts,
            documents,
            queries,
        }
    }
}

fn artifact(
    artifacts: &BTreeMap<String, ArtifactSpec>,
    name: &str,
    data_directory: &Path,
) -> PathBuf {
    let artifact = artifacts
        .get(name)
        .unwrap_or_else(|| panic!("missing prepared BEIR artifact {name}"));
    let relative = Path::new(&artifact.path);
    assert_eq!(relative.file_name(), Some(relative.as_os_str()));
    let path = data_directory.join(relative);
    assert_eq!(sha256_file(&path), artifact.sha256, "{name} artifact hash");
    path
}

fn read_documents(path: &Path, dimensions: usize) -> Vec<DocumentCase> {
    let raw: Vec<RawDocument> = read_json_lines(path);
    for (index, document) in raw.iter().enumerate() {
        assert_eq!(document.id, index as u64 + 1, "BEIR document IDs");
        assert!(!document.source_id.is_empty());
        assert!(!document.body.is_empty());
        validate_embedding(&document.embedding, dimensions, "document", index);
    }
    raw.into_iter()
        .map(|document| DocumentCase {
            id: document.id,
            source_id: document.source_id,
            body: document.body,
            embedding: document.embedding,
        })
        .collect()
}

fn read_queries(path: &Path, dimensions: usize, corpus_count: usize) -> Vec<QueryCase> {
    let raw: Vec<RawQuery> = read_json_lines(path);
    let mut query_ids = BTreeSet::new();
    raw.into_iter()
        .enumerate()
        .map(|(index, query)| {
            assert!(query_ids.insert(query.id.clone()), "duplicate query ID");
            assert!(!query.text.is_empty());
            validate_embedding(&query.embedding, dimensions, "query", index);
            let judgments = query
                .judgments
                .into_iter()
                .map(|(document_id, score)| {
                    let document_id = document_id
                        .parse::<u64>()
                        .expect("numeric BEIR document ID");
                    assert!((1..=corpus_count as u64).contains(&document_id));
                    assert!(score.is_finite() && score > 0.0);
                    (document_id, score)
                })
                .collect::<BTreeMap<_, _>>();
            assert!(!judgments.is_empty());
            QueryCase {
                id: query.id,
                judgments,
                params: [
                    SQLParam::scalar(Value::Str(query.text)),
                    SQLParam::vector(query.embedding),
                ],
            }
        })
        .collect()
}

fn read_json_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    let file = fs::File::open(path).expect("open prepared BEIR JSONL");
    BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(line_index, line)| {
            serde_json::from_str(&line.expect("read prepared BEIR JSONL line"))
                .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_index + 1))
        })
        .collect()
}

fn validate_embedding(vector: &[f32], dimensions: usize, kind: &str, index: usize) {
    assert_eq!(vector.len(), dimensions, "{kind} {index} dimensions");
    assert!(vector.iter().all(|value| value.is_finite()));
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() <= 1.0e-3, "{kind} {index} norm {norm}");
}

fn sha256_file(path: &Path) -> String {
    let mut file = fs::File::open(path).expect("open prepared BEIR artifact for hashing");
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).expect("hash prepared BEIR artifact");
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
