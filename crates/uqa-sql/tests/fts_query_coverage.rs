//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexer/parser coverage for `test_fts_match`.

use uqa_operators::OperatorTree;
use uqa_sql::{compile_fts_node, fts_tokenize, FTSNode, FTSParser, FTSTokenType};

fn whitespace_tokenizer(_field: Option<&str>, phrase: &str) -> Vec<String> {
    phrase
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect()
}

fn parse(query: &str) -> FTSNode {
    FTSParser::new(fts_tokenize(query).unwrap())
        .parse()
        .unwrap()
}

#[test]
fn test_single_term_token() {
    let tokens = fts_tokenize("database").unwrap();
    assert_eq!(tokens[0].kind, FTSTokenType::Term);
    assert_eq!(tokens[0].value, "database");
    assert_eq!(tokens[1].kind, FTSTokenType::Eof);
}

#[test]
fn test_multiple_terms_tokenized() {
    let tokens = fts_tokenize("database search engine").unwrap();
    assert_eq!(
        tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
        vec![
            FTSTokenType::Term,
            FTSTokenType::Term,
            FTSTokenType::Term,
            FTSTokenType::Eof
        ]
    );
}

#[test]
fn test_phrase_token() {
    let tokens = fts_tokenize(r#""information retrieval""#).unwrap();
    assert_eq!(tokens[0].kind, FTSTokenType::Phrase);
    assert_eq!(tokens[0].value, "information retrieval");
}

#[test]
fn test_vector_token() {
    let tokens = fts_tokenize("[0.1, 0.2, 0.3]").unwrap();
    assert_eq!(tokens[0].kind, FTSTokenType::Vector);
    assert_eq!(tokens[0].value, "0.1, 0.2, 0.3");
}

#[test]
fn test_boolean_keywords_case_insensitive() {
    let tokens = fts_tokenize("database AnD search oR NOT engine").unwrap();
    assert_eq!(tokens[1].kind, FTSTokenType::And);
    assert_eq!(tokens[3].kind, FTSTokenType::Or);
    assert_eq!(tokens[4].kind, FTSTokenType::Not);
}

#[test]
fn test_field_colon_term_tokens() {
    let tokens = fts_tokenize("title:database").unwrap();
    assert_eq!(
        tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
        vec![
            FTSTokenType::Term,
            FTSTokenType::Colon,
            FTSTokenType::Term,
            FTSTokenType::Eof
        ]
    );
}

#[test]
fn test_parentheses_tokens() {
    let tokens = fts_tokenize("(database OR search) AND engine").unwrap();
    assert_eq!(tokens[0].kind, FTSTokenType::LParen);
    assert_eq!(tokens[4].kind, FTSTokenType::RParen);
}

#[test]
fn test_empty_string_tokenizes_to_eof() {
    let tokens = fts_tokenize("").unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, FTSTokenType::Eof);
}

#[test]
fn test_unterminated_quote_errors() {
    let err = fts_tokenize(r#""unterminated"#).unwrap_err();
    assert!(err.to_string().contains("Unterminated quoted phrase"));
}

#[test]
fn test_unterminated_bracket_errors() {
    let err = fts_tokenize("[0.1, 0.2").unwrap_err();
    assert!(err.to_string().contains("Unterminated vector literal"));
}

#[test]
fn test_parse_single_term() {
    match parse("database") {
        FTSNode::Term { field, term } => {
            assert_eq!(field, None);
            assert_eq!(term, "database");
        }
        other => panic!("expected term, got {other:?}"),
    }
}

#[test]
fn test_parse_phrase() {
    match parse(r#""information retrieval""#) {
        FTSNode::Phrase { field, phrase } => {
            assert_eq!(field, None);
            assert_eq!(phrase, "information retrieval");
        }
        other => panic!("expected phrase, got {other:?}"),
    }
}

#[test]
fn test_parse_field_term() {
    match parse("title:database") {
        FTSNode::Term { field, term } => {
            assert_eq!(field.as_deref(), Some("title"));
            assert_eq!(term, "database");
        }
        other => panic!("expected field term, got {other:?}"),
    }
}

#[test]
fn test_parse_field_phrase() {
    match parse(r#"body:"information retrieval""#) {
        FTSNode::Phrase { field, phrase } => {
            assert_eq!(field.as_deref(), Some("body"));
            assert_eq!(phrase, "information retrieval");
        }
        other => panic!("expected field phrase, got {other:?}"),
    }
}

#[test]
fn test_parse_field_vector() {
    match parse("embedding:[0.1, 0.2, 0.3]") {
        FTSNode::Vector { field, values } => {
            assert_eq!(field.as_deref(), Some("embedding"));
            assert_eq!(values, vec![0.1, 0.2, 0.3]);
        }
        other => panic!("expected field vector, got {other:?}"),
    }
}

#[test]
fn test_parse_explicit_and() {
    assert!(matches!(parse("database AND search"), FTSNode::And(_, _)));
}

#[test]
fn test_parse_explicit_or() {
    assert!(matches!(parse("database OR search"), FTSNode::Or(_, _)));
}

#[test]
fn test_parse_not() {
    assert!(matches!(parse("NOT database"), FTSNode::Not(_)));
}

#[test]
fn test_parse_implicit_and() {
    assert!(matches!(parse("database search"), FTSNode::And(_, _)));
}

#[test]
fn test_precedence_and_over_or() {
    match parse("a OR b AND c") {
        FTSNode::Or(_, right) => assert!(matches!(*right, FTSNode::And(_, _))),
        other => panic!("expected OR with AND right child, got {other:?}"),
    }
}

#[test]
fn test_grouping_overrides_precedence() {
    match parse("(a OR b) AND c") {
        FTSNode::And(left, _) => assert!(matches!(*left, FTSNode::Or(_, _))),
        other => panic!("expected AND with OR left child, got {other:?}"),
    }
}

#[test]
fn test_double_negation() {
    match parse("NOT NOT database") {
        FTSNode::Not(inner) => assert!(matches!(*inner, FTSNode::Not(_))),
        other => panic!("expected nested NOT, got {other:?}"),
    }
}

#[test]
fn test_empty_query_errors() {
    let err = FTSParser::new(fts_tokenize("").unwrap())
        .parse()
        .unwrap_err();
    assert!(err.to_string().contains("Empty query"));
}

#[test]
fn test_trailing_operator_errors() {
    let err = FTSParser::new(fts_tokenize("database AND").unwrap())
        .parse()
        .unwrap_err();
    assert!(err.to_string().contains("Unexpected token"));
}

#[test]
fn test_unbalanced_paren_errors() {
    let err = FTSParser::new(fts_tokenize("(a AND b").unwrap())
        .parse()
        .unwrap_err();
    assert!(err.to_string().contains("Expected RParen"));
}

#[test]
fn test_three_implicit_and_left_associative() {
    match parse("a b c") {
        FTSNode::And(left, _) => assert!(matches!(*left, FTSNode::And(_, _))),
        other => panic!("expected left-associative AND, got {other:?}"),
    }
}

#[test]
fn test_compile_mixed_text_vector_and_uses_log_odds() {
    let ast = parse("body:search AND embedding:[0.1, 0.9, 0.0]");
    let op = compile_fts_node(&ast, Some("_all"), &whitespace_tokenizer);
    assert!(matches!(op, OperatorTree::LogOddsFusion { .. }));
}

#[test]
fn test_compile_all_field_resolves_to_none() {
    let ast = parse("database");
    let op = compile_fts_node(&ast, Some("_all"), &whitespace_tokenizer);
    match op {
        OperatorTree::Term { field, .. } => assert!(field.is_none()),
        _ => panic!("expected term"),
    }
}
