//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `QueryBuilder` fluent API.

use uqa_api::{Order, QueryBuilder};
use uqa_core::Value;
use uqa_engine::Engine;

fn engine_with_corpus() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, qty INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_notes_title ON notes USING gin (title)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO notes (id, title, qty) VALUES \
             (1, 'rust async', 7), \
             (2, 'python web', 3), \
             (3, 'rust embedded', 12), \
             (4, 'go networking', 5)",
            &[],
        )
        .unwrap();
    engine
}

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (
                id SERIAL PRIMARY KEY,
                title TEXT,
                body TEXT,
                year INTEGER,
                score REAL
            )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_docs_gin ON docs USING gin (title, body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (title, body, year, score) VALUES
             ('attention is all you need', 'transformer model uses self attention', 2017, 9.5),
             ('bert pre-training', 'bidirectional encoder representations', 2019, 8.0),
             ('graph attention networks', 'attention on graph structured data', 2018, 7.5),
             ('vision transformer', 'image recognition with patches', 2021, 6.0),
             ('scaling language models', 'scaling laws for neural language models', 2020, 8.5)",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn select_with_text_match_and_order_runs() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "title"])
        .text_match("title", "rust")
        .order_by_desc("_score")
        .limit(5)
        .execute()
        .unwrap();
    let titles: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("title") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(titles.iter().any(|t| t.contains("rust")));
}

#[test]
fn where_filters_compose_with_and() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .where_gt("qty", &Value::Int(4))
        .where_lt("qty", &Value::Int(10))
        .order_by_asc("id")
        .execute()
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    // qty range (4, 10) -> rows with qty=7 and qty=5 -> ids 1 and 4.
    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn to_sql_renders_full_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "title"])
        .where_eq("id", &Value::Int(2))
        .order_by("id", Order::Asc)
        .limit(3)
        .offset(1)
        .to_sql();
    assert_eq!(
        sql,
        "SELECT id, title FROM notes WHERE id = 2 ORDER BY id ASC LIMIT 3 OFFSET 1"
    );
}

#[test]
fn multi_field_match_through_builder() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .multi_field_match(&[("title", "rust"), ("title", "embedded")])
        .order_by_desc("_score")
        .execute()
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn fuse_attention_renders_attention_call() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .fuse_attention(&["text_match('rust')", "text_match('embedded')"])
        .to_sql();
    assert!(sql.contains("attention(text_match('rust'), text_match('embedded'))"));
}

#[test]
fn fuse_learned_quotes_model_name_and_includes_signals() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .fuse_learned("my_model", &["text_match('a')", "knn_match('v', '[1]', 5)"])
        .to_sql();
    assert!(sql.contains("learned_fusion('my_model', text_match('a'), knn_match('v', '[1]', 5))"));
}

#[test]
fn rpq_replaces_from_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "_unused")
        .select_columns(&["*"])
        .rpq("manages*", 1, "g")
        .to_sql();
    assert!(sql.contains("FROM rpq('manages*', 1, 'g')"));
}

#[test]
fn highlight_and_facets_render_function_calls() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .highlight("title", "rust")
        .facets(&["author", "year"])
        .to_sql();
    assert!(sql.contains("uqa_highlight('title', 'rust')"));
    assert!(sql.contains("uqa_facets('author', 'year')"));
}

#[test]
fn bayesian_match_renders_where_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .bayesian_match("title", "rust")
        .to_sql();
    assert!(sql.contains("WHERE bayesian_match(title, 'rust')"));
}

#[test]
fn explain_returns_plan_lines() {
    let engine = engine_with_corpus();
    let plan = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .where_eq("id", &Value::Int(1))
        .limit(2)
        .explain()
        .unwrap();
    assert!(plan.contains("Select"));
    assert!(plan.contains("limit=2"));
}

#[test]
fn execute_arrow_basic() {
    let engine = engine_with_docs();
    let batch = QueryBuilder::new(&engine, "docs")
        .term("graph", Some("title"))
        .execute_arrow()
        .unwrap();
    let schema = batch.schema();
    assert!(schema.field_with_name("_doc_id").is_ok());
    assert!(schema.field_with_name("_score").is_ok());
    assert!(batch.num_rows() >= 1);
}

#[test]
fn execute_arrow_empty_result_keeps_metadata_columns() {
    let engine = engine_with_docs();
    let batch = QueryBuilder::new(&engine, "docs")
        .term("xyznonexistent", Some("title"))
        .execute_arrow()
        .unwrap();
    let schema = batch.schema();
    assert_eq!(batch.num_rows(), 0);
    assert!(schema.field_with_name("_doc_id").is_ok());
    assert!(schema.field_with_name("_score").is_ok());
}

#[test]
fn execute_arrow_with_scoring_keeps_positive_match_scores() {
    let engine = engine_with_docs();
    let batch = QueryBuilder::new(&engine, "docs")
        .term("graph", Some("title"))
        .score_bm25("graph", None)
        .execute_arrow()
        .unwrap();
    let scores = batch
        .column_by_name("_score")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Float64Array>()
        .unwrap();
    assert!((0..scores.len()).all(|idx| scores.value(idx) > 0.0));
}

#[test]
fn execute_arrow_metadata_column_types() {
    let engine = engine_with_docs();
    let batch = QueryBuilder::new(&engine, "docs")
        .term("graph", Some("title"))
        .execute_arrow()
        .unwrap();
    let schema = batch.schema();
    assert_eq!(
        schema.field_with_name("_doc_id").unwrap().data_type(),
        &arrow_schema::DataType::Int64
    );
    assert_eq!(
        schema.field_with_name("_score").unwrap().data_type(),
        &arrow_schema::DataType::Float64
    );
}

#[test]
fn execute_parquet_basic() {
    let engine = engine_with_docs();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fluent.parquet");
    QueryBuilder::new(&engine, "docs")
        .term("graph", Some("title"))
        .execute_parquet(&path)
        .unwrap();
    let file = std::fs::File::open(path).unwrap();
    let builder =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
    let schema = builder.schema().clone();
    let batches: Vec<_> = builder.build().unwrap().collect::<Result<_, _>>().unwrap();
    let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert!(rows >= 1);
    assert!(schema.field_with_name("_doc_id").is_ok());
    assert!(schema.field_with_name("_score").is_ok());
}

#[test]
fn execute_parquet_roundtrip_matches_arrow_doc_ids() {
    let engine = engine_with_docs();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roundtrip.parquet");
    let query = QueryBuilder::new(&engine, "docs").term("graph", Some("title"));
    query.execute_parquet(&path).unwrap();

    let arrow_batch = query.execute_arrow().unwrap();
    let arrow_ids = arrow_batch
        .column_by_name("_doc_id")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();

    let file = std::fs::File::open(path).unwrap();
    let mut reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let parquet_batch = reader.next().unwrap().unwrap();
    let parquet_ids = parquet_batch
        .column_by_name("_doc_id")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();

    assert_eq!(arrow_batch.num_rows(), parquet_batch.num_rows());
    assert_eq!(arrow_ids.values(), parquet_ids.values());
}

#[test]
fn execute_parquet_empty() {
    let engine = engine_with_docs();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.parquet");
    QueryBuilder::new(&engine, "docs")
        .term("xyznonexistent", Some("title"))
        .execute_parquet(&path)
        .unwrap();
    let file = std::fs::File::open(path).unwrap();
    let batches: Vec<_> =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
    let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
    assert_eq!(rows, 0);
}

#[test]
fn fluent_api_with_authority_prior() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE articles (id SERIAL PRIMARY KEY, body TEXT, source TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_articles_gin ON articles USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO articles (body, source) VALUES
             ('information retrieval systems', 'high'),
             ('retrieval augmented generation', 'low')",
            &[],
        )
        .unwrap();

    let result = QueryBuilder::new(&engine, "articles")
        .score_bayesian_with_prior("retrieval", Some("body"), Some("source"), Some("authority"))
        .unwrap()
        .execute()
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn fluent_api_requires_prior_fn() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id SERIAL PRIMARY KEY, text TEXT)", &[])
        .unwrap();
    let err = match QueryBuilder::new(&engine, "t").score_bayesian_with_prior(
        "test",
        Some("text"),
        None,
        None,
    ) {
        Ok(_) => panic!("expected missing prior error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("prior_fn is required"));
}

#[test]
fn query_builder_learn_params() {
    let engine = engine_with_docs();
    let result = QueryBuilder::new(&engine, "docs")
        .learn_params("learning", &[1, 1, 0, 0, 0], Some("body"))
        .unwrap();
    assert!(result.contains_key("alpha"));
}

#[test]
fn query_builder_sparse_threshold() {
    let engine = engine_with_docs();
    let result = QueryBuilder::new(&engine, "docs")
        .term("learning", Some("body"))
        .score_bayesian_bm25("learning", Some("body"))
        .sparse_threshold(0.3)
        .unwrap()
        .execute()
        .unwrap();
    for row in result.rows {
        if let Some(Value::Float(score)) = row.get("_score") {
            assert!(*score > 0.0);
        }
    }
}

#[test]
fn query_builder_sparse_threshold_requires_source() {
    let engine = engine_with_docs();
    let err = match QueryBuilder::new(&engine, "docs").sparse_threshold(0.3) {
        Ok(_) => panic!("expected source error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("requires a source"));
}
