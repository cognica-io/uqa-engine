//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn vector(values: &[f64]) -> Value {
    Value::List(values.iter().copied().map(Value::Float).collect())
}

fn vector_index_kind(engine: &Engine, table: &str, field: &str) -> String {
    let table = engine.table(table).unwrap().expect("table");
    let indexes = table.vector_indexes.read();
    indexes
        .get(field)
        .expect("vector index")
        .index_kind()
        .into()
}

mod api_validation;

mod storage_consistency;

mod search_and_vectors;

mod catalog;
mod prepared;
mod queries;
