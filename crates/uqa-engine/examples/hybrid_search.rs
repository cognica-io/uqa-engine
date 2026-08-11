//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
// Run with: cargo run -p uqa-engine --example hybrid_search

use uqa_core::Value;
use uqa_engine::{Engine, HybridSearchParams};
use uqa_storage::document_store::Document;

fn main() {
    let engine = Engine::new();

    engine
        .create_default_table("articles", vec!["body".to_string()])
        .unwrap();
    engine
        .create_vector_field("articles", "embedding", 3)
        .unwrap();

    let corpus = [
        (
            1u64,
            "rust async story with futures and tokio runtime",
            [1.0, 0.1, 0.1],
        ),
        (
            2,
            "rust language guide a deep dive into generics and traits",
            [0.9, 0.2, 0.0],
        ),
        (
            3,
            "python web frameworks flask and django and tooling",
            [0.1, 1.0, 0.0],
        ),
        (
            4,
            "rust embedded systems on no_std targets and drivers",
            [0.8, 0.0, 0.4],
        ),
        (5, "go networking channels and goroutines", [0.0, 0.0, 1.0]),
    ];
    for (id, body, vec) in corpus {
        let mut doc = Document::new();
        doc.insert("body".into(), Value::Str(body.to_string()));
        engine
            .add_document_with_vectors(
                "articles",
                id,
                doc,
                [("embedding".to_string(), vec.to_vec())]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
    }

    let hits = engine
        .hybrid_search(&HybridSearchParams {
            table: "articles",
            text_field: "body",
            text_query: "rust async",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0, 0.0],
            knn_pool: 5,
            top_k: 3,
        })
        .unwrap();

    println!("Hybrid (text + vector, exact single-prior log-odds) top 3:");
    for h in &hits {
        println!("  doc_id={} score={:.4}", h.doc_id, h.score);
    }
}
