//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen inputs shared with the independently implemented Python verifier.

use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use uqa_sql::SQLParam;

const FROZEN_MANIFEST: &[u8] = include_bytes!(
    "../../../../../benchmarks/vector-search/fixtures/scifact-minilm-prefix512-v1/manifest.json"
);
const CORPUS: &[u8] = include_bytes!(
    "../../../../../benchmarks/vector-search/fixtures/scifact-minilm-prefix512-v1/corpus.f32"
);
const QUERIES: &[u8] = include_bytes!(
    "../../../../../benchmarks/vector-search/fixtures/scifact-minilm-prefix512-v1/queries.f32"
);

pub(super) struct Dataset {
    pub rows: Vec<SQLParam>,
    pub queries: Vec<Vec<f32>>,
    pub tensor: bool,
}

pub(super) fn count(value: &Json) -> usize {
    usize::try_from(value.as_u64().expect("nonnegative manifest integer"))
        .expect("manifest integer fits this platform")
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn vector(seed: u64, dimensions: usize) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..dimensions)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 32) as u32 as f32 / u32::MAX as f32 * 2.0 - 1.0
        })
        .collect()
}

fn decode(bytes: &[u8], expected: &Json, dimensions: usize) -> Vec<Vec<f32>> {
    assert_eq!(digest(bytes), expected["sha256"].as_str().unwrap());
    assert_eq!(bytes.len(), count(&expected["bytes"]));
    assert_eq!(bytes.len(), count(&expected["rows"]) * dimensions * 4);
    bytes
        .chunks_exact(dimensions * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|chunk| {
                    let value = f32::from_le_bytes(chunk.try_into().unwrap());
                    assert!(value.is_finite());
                    value
                })
                .collect()
        })
        .collect()
}

fn literal(value: &Json) -> Vec<f32> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|component| component.as_f64().unwrap() as f32)
        .collect()
}

pub(super) fn load(spec: &Json) -> Dataset {
    let dimensions = count(&spec["dimensions"]);
    let (rows, queries, tensor) = match spec["generator"].as_str().unwrap() {
        "lcg-uniform-signed-f32-v1" => (
            (0..count(&spec["corpus_size"]))
                .map(|row| {
                    SQLParam::vector(vector(
                        spec["corpus_seed_start"].as_u64().unwrap() + row as u64,
                        dimensions,
                    ))
                })
                .collect(),
            (0..count(&spec["query_count"]))
                .map(|query| {
                    vector(
                        spec["query_seed_start"].as_u64().unwrap() + query as u64,
                        dimensions,
                    )
                })
                .collect(),
            false,
        ),
        "frozen-f32-v1" => {
            assert_eq!(
                digest(FROZEN_MANIFEST),
                spec["fixture_manifest_sha256"].as_str().unwrap()
            );
            let frozen: Json = serde_json::from_slice(FROZEN_MANIFEST).unwrap();
            assert_eq!(count(&frozen["dimensions"]), dimensions);
            (
                decode(CORPUS, &frozen["artifacts"]["corpus"], dimensions)
                    .into_iter()
                    .map(SQLParam::vector)
                    .collect(),
                decode(QUERIES, &frozen["artifacts"]["queries"], dimensions),
                false,
            )
        }
        "literal-tensor-v1" => (
            spec["corpus"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    if row.is_null() {
                        SQLParam::scalar(uqa_core::Value::Null)
                    } else {
                        SQLParam::tensor(row.as_array().unwrap().iter().map(literal).collect())
                    }
                })
                .collect(),
            spec["queries"]
                .as_array()
                .unwrap()
                .iter()
                .map(literal)
                .collect(),
            true,
        ),
        other => panic!("unknown correctness fixture {other}"),
    };
    let dataset = Dataset {
        rows,
        queries,
        tensor,
    };
    assert_eq!(dataset.rows.len(), count(&spec["corpus_size"]));
    assert_eq!(dataset.queries.len(), count(&spec["query_count"]));
    dataset
}
