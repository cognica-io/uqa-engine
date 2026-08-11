//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for the `QueryBuilder` fluent API.

use std::collections::BTreeMap;

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

fn engine_with_vectors() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE vector_docs (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO vector_docs (id, embedding) VALUES \
             (1, ARRAY[0.9, 0.1]), \
             (2, ARRAY[0.8, 0.2]), \
             (3, ARRAY[0.1, 0.9])",
            &[],
        )
        .unwrap();
    engine
}

fn assert_probability_scores(result: &uqa_sql::SQLResult) {
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing probability _score: {other:?}"),
        }
    }
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
        .unwrap()
        .where_lt("qty", &Value::Int(10))
        .unwrap()
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
fn where_gte_and_lte_execute_inclusively() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .where_gte("qty", &Value::Int(5))
        .unwrap()
        .where_lte("qty", &Value::Int(7))
        .unwrap()
        .order_by_asc("id")
        .execute()
        .unwrap();
    let ids = result
        .rows
        .iter()
        .map(|row| row.get("id").cloned().expect("id projection"))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![Value::Int(1), Value::Int(4)]);
}

#[test]
fn to_sql_renders_full_clause() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "title"])
        .where_eq("id", &Value::Int(2))
        .unwrap()
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
fn value_filters_preserve_bytes_and_reject_unrepresentable_values() {
    let engine = engine_with_corpus();
    let sql = QueryBuilder::new(&engine, "notes")
        .where_eq("payload", &Value::Bytes(vec![0x00, 0xab, 0xff]))
        .unwrap()
        .to_sql();
    assert_eq!(
        sql,
        "SELECT * FROM notes WHERE payload = decode('00abff', 'hex')"
    );

    let map_error = QueryBuilder::new(&engine, "notes")
        .where_eq("payload", &Value::Map(BTreeMap::new()))
        .err()
        .expect("a map must not be silently rendered as NULL");
    assert!(map_error.to_string().contains("map filter values"));

    let float_error = QueryBuilder::new(&engine, "notes")
        .where_eq("score", &Value::Float(f64::NAN))
        .err()
        .expect("a non-finite value must not produce invalid SQL");
    assert!(float_error.to_string().contains("non-finite"));
}

#[test]
fn multi_field_match_through_builder() {
    let engine = engine_with_corpus();
    let result = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id"])
        .multi_field_match(&[("title", "rust"), ("title", "embedded")])
        .unwrap()
        .order_by_desc("_score")
        .execute()
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn multi_field_match_rejects_too_few_pairs_and_empty_fields() {
    let engine = engine_with_corpus();
    let arity_error = QueryBuilder::new(&engine, "notes")
        .multi_field_match(&[("title", "rust")])
        .err()
        .expect("one field/query pair must be rejected");
    assert!(arity_error.to_string().contains(">=2 field/query pairs"));

    let field_error = QueryBuilder::new(&engine, "notes")
        .multi_field_match(&[("title", "rust"), (" ", "embedded")])
        .err()
        .expect("an empty field name must be rejected");
    assert!(field_error
        .to_string()
        .contains("field names cannot be empty"));
}

#[test]
fn direct_staged_retrieval_executes_registered_field_query_form() {
    let engine = engine_with_corpus();
    let query = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .staged_retrieval(&[("title", "rust", 10), ("title", "embedded", 10)])
        .unwrap();
    assert!(query
        .to_sql()
        .contains(" WHERE staged_retrieval(title, 'rust', 10, title, 'embedded', 10)"));
    let result = query.execute().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(3)));
}

#[test]
fn fuse_log_odds_executes_generated_shared_ir_query() {
    let engine = engine_with_corpus();
    let query = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .fuse_log_odds(
            &[
                "bayesian_match(title, 'rust')",
                "bayesian_match(title, 'embedded')",
            ],
            Some(0.1),
        )
        .unwrap();
    assert!(query.to_sql().contains(" WHERE fuse_log_odds("));
    assert!(query.to_sql().contains("base_rate => 0.1"));
    assert_probability_scores(&query.execute().unwrap());
}

#[test]
fn explicit_evidence_fusion_builders_execute_distinct_contracts() {
    let engine = engine_with_corpus();
    let signals = [
        "bayesian_match(title, 'rust')",
        "bayesian_match(title, 'embedded')",
    ];
    let robust = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .pool_positive_evidence(&signals, 0.5)
        .unwrap();
    assert!(robust.to_sql().contains("pool_positive_evidence("));
    assert_probability_scores(&robust.execute().unwrap());

    let exact = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .fuse_bayesian_evidence(&signals, Some(0.1))
        .unwrap();
    assert!(exact.to_sql().contains("fuse_bayesian_evidence("));
    assert!(exact.to_sql().contains("base_rate => 0.1"));
    assert_probability_scores(&exact.execute().unwrap());
}

#[test]
fn multi_stage_executes_registered_staged_retrieval_query() {
    let engine = engine_with_corpus();
    let query = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .multi_stage(&[
            ("bayesian_match(title, 'rust')", 10),
            ("bayesian_match(title, 'embedded')", 10),
        ])
        .unwrap();
    assert!(query.to_sql().contains(" WHERE staged_retrieval("));
    assert_eq!(query.execute().unwrap().rows.len(), 1);
}

#[test]
fn fuse_attention_executes_generated_shared_ir_query() {
    let engine = engine_with_corpus();
    let query = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .fuse_attention(&[
            "bayesian_match(title, 'rust')",
            "bayesian_match(title, 'embedded')",
        ])
        .unwrap();
    assert!(query.to_sql().contains(" WHERE fuse_attention("));
    assert_probability_scores(&query.execute().unwrap());
}

#[test]
fn fuse_learned_executes_signals_with_optional_alpha() {
    let engine = engine_with_corpus();
    let signals = [
        "bayesian_match(title, 'rust')",
        "bayesian_match(title, 'embedded')",
    ];
    let with_alpha = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .fuse_learned(&signals, Some(0.7))
        .unwrap();
    assert!(with_alpha
        .to_sql()
        .contains(" WHERE fuse_learned(bayesian_match(title, 'rust'), bayesian_match(title, 'embedded'), alpha => 0.7)"));
    assert_probability_scores(&with_alpha.execute().unwrap());

    let default_alpha = QueryBuilder::new(&engine, "notes")
        .select_columns(&["id", "_score"])
        .fuse_learned(&signals, None)
        .unwrap();
    assert!(!default_alpha.to_sql().contains("alpha =>"));
    assert_probability_scores(&default_alpha.execute().unwrap());
}

#[test]
fn fusion_builders_reject_invalid_alpha_and_signal_arity() {
    let engine = engine_with_corpus();
    let signals = [
        "bayesian_match(title, 'rust')",
        "bayesian_match(title, 'embedded')",
    ];
    for alpha in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
        let robust_error = QueryBuilder::new(&engine, "notes")
            .pool_positive_evidence(&signals, alpha)
            .err()
            .expect("invalid robust-pool alpha must fail");
        assert!(robust_error.to_string().contains("finite and in [0, 1]"));

        let learned_error = QueryBuilder::new(&engine, "notes")
            .fuse_learned(&signals, Some(alpha))
            .err()
            .expect("invalid learned alpha must fail");
        assert!(learned_error.to_string().contains("finite and in [0, 1]"));
    }

    let one_signal = ["bayesian_match(title, 'rust')"];
    for error in [
        QueryBuilder::new(&engine, "notes")
            .fuse_log_odds(&one_signal, None)
            .err()
            .expect("log odds requires two signals"),
        QueryBuilder::new(&engine, "notes")
            .pool_positive_evidence(&one_signal, 0.5)
            .err()
            .expect("robust pool requires two signals"),
        QueryBuilder::new(&engine, "notes")
            .fuse_bayesian_evidence(&one_signal, None)
            .err()
            .expect("Bayesian evidence fusion requires two signals"),
        QueryBuilder::new(&engine, "notes")
            .fuse_attention(&one_signal)
            .err()
            .expect("attention requires two signals"),
        QueryBuilder::new(&engine, "notes")
            .fuse_learned(&one_signal, None)
            .err()
            .expect("learned fusion requires two signals"),
    ] {
        assert!(error.to_string().contains(">=2 signals"));
    }

    for invalid_prior in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, 1.0] {
        for error in [
            QueryBuilder::new(&engine, "notes")
                .fuse_log_odds(&signals, Some(invalid_prior))
                .err()
                .expect("invalid log-odds prior must fail"),
            QueryBuilder::new(&engine, "notes")
                .fuse_bayesian_evidence(&signals, Some(invalid_prior))
                .err()
                .expect("invalid Bayesian evidence prior must fail"),
        ] {
            assert!(error.to_string().contains("base_rate"));
        }
    }

    let no_signal_stages: [(&str, usize); 0] = [];
    let multi_stage_error = QueryBuilder::new(&engine, "notes")
        .multi_stage(&no_signal_stages)
        .err()
        .expect("multi-stage retrieval requires a stage");
    assert!(multi_stage_error.to_string().contains(">=1 stage"));

    let no_field_stages: [(&str, &str, usize); 0] = [];
    let staged_error = QueryBuilder::new(&engine, "notes")
        .staged_retrieval(&no_field_stages)
        .err()
        .expect("staged retrieval requires a stage");
    assert!(staged_error.to_string().contains(">=1 stage"));
}

#[test]
fn calibrated_vector_builder_executes_through_where_ir() {
    let engine = engine_with_vectors();
    let query = QueryBuilder::new(&engine, "vector_docs")
        .select_columns(&["id", "_score"])
        .calibrated_vector_match("embedding", &[0.9, 0.1], 3, Some(0.0))
        .unwrap()
        .order_by_desc("_score");
    assert!(query
        .to_sql()
        .contains(" WHERE calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3, 0)"));
    assert_probability_scores(&query.execute().unwrap());
}

#[test]
fn vector_builders_reject_invalid_vectors_k_and_thresholds() {
    let engine = engine_with_vectors();
    for vector in [&[][..], &[f32::NAN, 0.1][..], &[f32::INFINITY, 0.1][..]] {
        let error = QueryBuilder::new(&engine, "vector_docs")
            .calibrated_vector_match("embedding", vector, 3, None)
            .err()
            .expect("invalid vector must fail");
        assert!(error.to_string().contains("non-empty finite query vector"));
    }

    for k in [0, usize::MAX] {
        let error = QueryBuilder::new(&engine, "vector_docs")
            .calibrated_vector_match("embedding", &[0.9, 0.1], k, None)
            .err()
            .expect("invalid k must fail");
        assert!(error.to_string().contains("positive"));
    }

    for threshold in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
        let error = QueryBuilder::new(&engine, "vector_docs")
            .calibrated_vector_match("embedding", &[0.9, 0.1], 3, Some(threshold))
            .err()
            .expect("invalid threshold must fail");
        assert!(error.to_string().contains("finite and in [0, 1]"));
    }

    let empty_field = QueryBuilder::new(&engine, "vector_docs")
        .calibrated_vector_match(" ", &[0.9, 0.1], 3, None)
        .err()
        .expect("empty vector field must fail");
    assert!(empty_field
        .to_string()
        .contains("field name cannot be empty"));
}

#[test]
fn vector_compatibility_builder_emits_registered_knn_query() {
    let engine = engine_with_vectors();
    let query = QueryBuilder::new(&engine, "vector_docs")
        .select_columns(&["id", "_score"])
        .vector(&[0.9, 0.1], 3, "embedding")
        .unwrap();
    assert!(query.to_sql().contains(" WHERE knn_match("));
    assert!(!query.execute().unwrap().rows.is_empty());
}

#[test]
fn direct_knn_match_executes_registered_retrieval_predicate() {
    let engine = engine_with_vectors();
    let query = QueryBuilder::new(&engine, "vector_docs")
        .select_columns(&["id", "_score"])
        .knn_match("embedding", &[0.9, 0.1], 2)
        .unwrap()
        .order_by_desc("_score");
    assert!(query
        .to_sql()
        .contains(" WHERE knn_match(embedding, ARRAY[0.9, 0.1], 2)"));
    let result = query.execute().unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
}

#[test]
fn all_field_term_and_facet_builders_execute_their_generated_sql() {
    let docs = engine_with_docs();
    let all_fields = QueryBuilder::new(&docs, "docs")
        .select_columns(&["id", "_score"])
        .term("attention", None);
    assert!(all_fields
        .to_sql()
        .contains("fts_match('_all', 'attention')"));
    assert!(!all_fields.execute().unwrap().rows.is_empty());

    let notes = engine_with_corpus();
    let facet = QueryBuilder::new(&notes, "notes").facet("qty");
    assert!(facet.to_sql().contains(" GROUP BY qty"));
    assert_eq!(facet.execute().unwrap().rows.len(), 4);
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
fn highlight_and_facets_execute_registered_projection_forms() {
    let engine = engine_with_docs();
    let highlighted = QueryBuilder::new(&engine, "docs")
        .highlight("body", "attention")
        .unwrap();
    assert!(highlighted
        .to_sql()
        .contains("uqa_highlight(body, 'attention')"));
    assert!(highlighted.execute().unwrap().rows.iter().any(|row| {
        row.values()
            .any(|value| matches!(value, Value::Str(text) if text.contains("<b>attention</b>")))
    }));

    let facets = QueryBuilder::new(&engine, "docs")
        .facets(&["year"])
        .unwrap();
    assert!(facets.to_sql().contains("uqa_facets(year)"));
    assert_eq!(facets.execute().unwrap().rows.len(), 5);

    let empty_fields: [&str; 0] = [];
    let error = QueryBuilder::new(&engine, "docs")
        .facets(&empty_fields)
        .err()
        .expect("facets requires at least one field");
    assert!(error.to_string().contains(">=1 field"));

    let highlight_error = QueryBuilder::new(&engine, "docs")
        .highlight(" ", "attention")
        .err()
        .expect("highlight requires a non-empty field");
    assert!(highlight_error
        .to_string()
        .contains("field name cannot be empty"));
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
        .unwrap()
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
