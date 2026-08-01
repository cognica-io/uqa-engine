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

#[path = "lib_tests/api_validation.rs"]
mod api_validation;

#[path = "lib_tests/storage_consistency.rs"]
mod storage_consistency;

#[path = "lib_tests/search_and_vectors.rs"]
mod search_and_vectors;
