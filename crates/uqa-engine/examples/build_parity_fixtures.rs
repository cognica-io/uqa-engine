//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Refresh the committed text-search and hybrid-search parity fixtures.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uqa_core::{FieldName, Value};
use uqa_engine::{Engine, HybridSearchParams, ScoringMode};
use uqa_scoring::{BM25Params, BayesianBM25Params};
use uqa_storage::document_store::Document;

const FIXTURE_VERSION: u32 = 2;

#[derive(Deserialize, Serialize)]
struct TextFixture {
    version: u32,
    corpus: Vec<TextDoc>,
    queries: Vec<TextQuery>,
}

#[derive(Deserialize, Serialize)]
struct TextDoc {
    id: u64,
    title: String,
    body: String,
}

#[derive(Deserialize, Serialize)]
struct TextQuery {
    field: String,
    query: String,
    scoring: String,
    top_k: usize,
    expected: Vec<FixtureHit>,
}

#[derive(Deserialize, Serialize)]
struct HybridFixture {
    version: u32,
    vector_dim: u32,
    corpus: Vec<HybridDoc>,
    queries: Vec<HybridQuery>,
}

#[derive(Deserialize, Serialize)]
struct HybridDoc {
    id: u64,
    title: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize, Serialize)]
struct HybridQuery {
    text_field: String,
    text_query: String,
    query_vector: Vec<f32>,
    knn_pool: usize,
    alpha: f64,
    top_k: usize,
    expected: Vec<FixtureHit>,
}

#[derive(Deserialize, Serialize)]
struct FixtureHit {
    doc_id: u64,
    score: f64,
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("uqa-engine has a crates parent")
        .parent()
        .expect("crates has a workspace parent")
        .join("tests")
        .join("parity")
}

fn scoring_mode(name: &str) -> ScoringMode {
    match name {
        "bm25" => ScoringMode::BM25(BM25Params::default()),
        "bayesian_bm25" => ScoringMode::BayesianBM25(BayesianBM25Params::default()),
        other => panic!("unknown scoring mode in fixture: {other}"),
    }
}

fn write_fixture<T: Serialize>(path: &Path, fixture: &T) -> Result<(), Box<dyn Error>> {
    let mut json = serde_json::to_string_pretty(fixture)?;
    json.push('\n');
    std::fs::write(path, json)?;
    Ok(())
}

fn refresh_text_fixture(path: &Path) -> Result<usize, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let mut fixture: TextFixture = serde_json::from_slice(&bytes)?;

    let engine = Engine::new();
    engine.create_default_table("articles", vec!["title".into(), "body".into()])?;
    for corpus_doc in &fixture.corpus {
        let mut document = Document::new();
        document.insert("title".into(), Value::Str(corpus_doc.title.clone()));
        document.insert("body".into(), Value::Str(corpus_doc.body.clone()));
        engine.add_document("articles", corpus_doc.id, document)?;
    }

    fixture.version = FIXTURE_VERSION;
    for query in &mut fixture.queries {
        query.expected = engine
            .search(
                "articles",
                &query.field,
                &query.query,
                &scoring_mode(&query.scoring),
                query.top_k,
            )?
            .into_iter()
            .map(|hit| FixtureHit {
                doc_id: hit.doc_id,
                score: hit.score,
            })
            .collect();
    }

    let query_count = fixture.queries.len();
    write_fixture(path, &fixture)?;
    Ok(query_count)
}

fn refresh_hybrid_fixture(path: &Path) -> Result<usize, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let mut fixture: HybridFixture = serde_json::from_slice(&bytes)?;

    let engine = Engine::new();
    engine.create_default_table("articles", vec!["title".into()])?;
    engine.create_vector_field("articles", "embedding", fixture.vector_dim)?;
    for corpus_doc in &fixture.corpus {
        let mut document = Document::new();
        document.insert("title".into(), Value::Str(corpus_doc.title.clone()));
        let mut vectors: BTreeMap<FieldName, Vec<f32>> = BTreeMap::new();
        vectors.insert("embedding".into(), corpus_doc.embedding.clone());
        engine.add_document_with_vectors("articles", corpus_doc.id, document, vectors)?;
    }

    fixture.version = FIXTURE_VERSION;
    for query in &mut fixture.queries {
        query.expected = engine
            .hybrid_search(&HybridSearchParams {
                table: "articles",
                text_field: &query.text_field,
                text_query: &query.text_query,
                vector_field: "embedding",
                query_vector: query.query_vector.clone(),
                knn_pool: query.knn_pool,
                alpha: query.alpha,
                top_k: query.top_k,
            })?
            .into_iter()
            .map(|hit| FixtureHit {
                doc_id: hit.doc_id,
                score: hit.score,
            })
            .collect();
    }

    let query_count = fixture.queries.len();
    write_fixture(path, &fixture)?;
    Ok(query_count)
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = fixture_directory();
    let text_path = directory.join("text_search_fixture.json");
    let hybrid_path = directory.join("hybrid_search_fixture.json");

    let text_query_count = refresh_text_fixture(&text_path)?;
    let hybrid_query_count = refresh_hybrid_fixture(&hybrid_path)?;

    println!("refreshed {text_query_count} text queries and {hybrid_query_count} hybrid queries");
    Ok(())
}
