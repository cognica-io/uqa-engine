//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL diagnostics for complete analyzer token streams.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::ColumnType;

#[test]
fn analyze_text_returns_jsonb_with_positions_offsets_and_revision() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT analysis FROM analyze_text('standard', 'The Cats')",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, ["analysis"]);
    assert_eq!(result.column_types, [Some(ColumnType::JsonB)]);
    let Value::JsonB(json) = &result.rows[0]["analysis"] else {
        panic!("analyze_text must return JSONB");
    };
    let diagnostic: serde_json::Value = serde_json::from_str(json).unwrap();
    let fingerprint = diagnostic["analyzer_fingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 64);
    let expected = uqa_analysis::get_analyzer("standard")
        .unwrap()
        .compile()
        .unwrap()
        .descriptor()
        .fingerprint()
        .to_string();
    assert_eq!(fingerprint, expected);
    assert!(diagnostic["final_position_increment"].is_number());
    assert!(diagnostic["final_offsets"]["utf8"].is_object());
    assert!(diagnostic["final_offsets"]["utf16"].is_object());
    let tokens = diagnostic["tokens"].as_array().unwrap();
    assert!(!tokens.is_empty());
    assert!(tokens[0]["term"].is_string());
    assert!(tokens[0]["offsets"]["utf8"].is_object());
    assert!(tokens[0]["offsets"]["utf16"].is_object());
    assert!(tokens[0]["position_increment"].is_number());
    assert!(tokens[0]["position_length"].is_number());
}

#[test]
fn analyze_text_supports_aliases_and_feature_disabled_nori_is_an_error() {
    let eng = Engine::new();
    let aliased = eng
        .sql(
            "SELECT token_dump FROM analyze_text('keyword', 'UQA') AS a(token_dump)",
            &[],
        )
        .unwrap();
    assert_eq!(aliased.columns, ["token_dump"]);
    assert!(matches!(aliased.rows[0]["token_dump"], Value::JsonB(_)));

    if uqa_analysis::get_analyzer("nori").is_ok() {
        let result = eng
            .sql("SELECT analysis FROM analyze_text('nori', '나물은')", &[])
            .unwrap();
        let Value::JsonB(json) = &result.rows[0]["analysis"] else {
            panic!("nori analyze_text must return JSONB");
        };
        let diagnostic: serde_json::Value = serde_json::from_str(json).unwrap();
        assert!(diagnostic["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|token| token.get("korean_morphology").is_some()));
    } else {
        let error = eng
            .sql("SELECT * FROM analyze_text('nori', '나물은')", &[])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("analyzer `nori` is not registered"));
    }
}

#[test]
fn analyze_text_rejects_wrong_arity_and_argument_types() {
    let eng = Engine::new();
    let arity = eng
        .sql("SELECT * FROM analyze_text('standard')", &[])
        .unwrap_err();
    assert!(arity.to_string().contains("analyze_text"));
    let type_error = eng
        .sql("SELECT * FROM analyze_text('standard', 42)", &[])
        .unwrap_err();
    assert!(type_error.to_string().contains("analyze_text arg 2"));
}
