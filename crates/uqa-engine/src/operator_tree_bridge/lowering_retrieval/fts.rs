//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-owned lowering from the syntax-only FTS AST to retrieval operators.

use uqa_operators::{GatingSpec, OperatorTree};
use uqa_sql::{FTSNode, SQLError};

const VECTOR_K: usize = 10_000;

pub(super) fn compile_query_string(
    query: &str,
    default_field: Option<&str>,
) -> Result<OperatorTree, SQLError> {
    let ast = uqa_sql::parse_fts_query_string(query)?;
    Ok(compile(&ast, default_field, &tokenize_phrase))
}

fn tokenize_phrase(_field: Option<&str>, phrase: &str) -> Vec<String> {
    phrase
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect()
}

fn compile(
    node: &FTSNode,
    default_field: Option<&str>,
    phrase_tokenizer: &dyn Fn(Option<&str>, &str) -> Vec<String>,
) -> OperatorTree {
    match node {
        FTSNode::Term { field, term } => {
            term_operator(term.clone(), resolve_field(field.as_deref(), default_field))
        }
        FTSNode::Phrase { field, phrase } => {
            let resolved = resolve_field(field.as_deref(), default_field);
            let terms = phrase_tokenizer(resolved.as_deref(), phrase);
            compile_phrase(terms, resolved)
        }
        FTSNode::Vector { field, values } => OperatorTree::KNN {
            query_vector: values.clone(),
            k: VECTOR_K,
            field: resolve_field(field.as_deref(), default_field)
                .unwrap_or_else(|| "embedding".into()),
        },
        FTSNode::And(left, right) => compile_and(left, right, default_field, phrase_tokenizer),
        FTSNode::Or(left, right) => OperatorTree::Union(vec![
            compile(left, default_field, phrase_tokenizer),
            compile(right, default_field, phrase_tokenizer),
        ]),
        FTSNode::Not(operand) => {
            OperatorTree::Complement(Box::new(compile(operand, default_field, phrase_tokenizer)))
        }
    }
}

fn compile_phrase(terms: Vec<String>, field: Option<String>) -> OperatorTree {
    match terms.as_slice() {
        [] => OperatorTree::Empty,
        [query] => term_operator(query.clone(), field),
        _ => OperatorTree::Intersect(
            terms
                .into_iter()
                .map(|query| term_operator(query, field.clone()))
                .collect(),
        ),
    }
}

fn compile_and(
    left: &FTSNode,
    right: &FTSNode,
    default_field: Option<&str>,
    phrase_tokenizer: &dyn Fn(Option<&str>, &str) -> Vec<String>,
) -> OperatorTree {
    let signals = vec![
        compile(left, default_field, phrase_tokenizer),
        compile(right, default_field, phrase_tokenizer),
    ];
    if has_vector_signal(left) ^ has_vector_signal(right) {
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha: 0.5,
            gating: GatingSpec::Softplus,
            weights: None,
            logit_min: None,
            logit_max: None,
            adaptive_weights: false,
        }
    } else {
        OperatorTree::Intersect(signals)
    }
}

fn term_operator(query: String, field: Option<String>) -> OperatorTree {
    OperatorTree::Term {
        query,
        field,
        scoring: None,
        top_k: None,
    }
}

fn resolve_field(node_field: Option<&str>, default_field: Option<&str>) -> Option<String> {
    match node_field.or(default_field) {
        Some("_all") | None => None,
        Some(field) => Some(field.to_string()),
    }
}

fn has_vector_signal(node: &FTSNode) -> bool {
    match node {
        FTSNode::Vector { .. } => true,
        FTSNode::Term { .. } | FTSNode::Phrase { .. } => false,
        FTSNode::And(left, right) | FTSNode::Or(left, right) => {
            has_vector_signal(left) || has_vector_signal(right)
        }
        FTSNode::Not(inner) => has_vector_signal(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_is_lowered_after_engine_tokenization() {
        let tree = compile_query_string("body:\"Rust Ferris Crab\"", None).unwrap();
        let OperatorTree::Intersect(terms) = tree else {
            panic!("expected phrase terms to intersect");
        };
        assert_eq!(terms.len(), 3);
        assert!(terms.iter().all(|term| matches!(
            term,
            OperatorTree::Term {
                field: Some(field),
                scoring: None,
                ..
            } if field == "body"
        )));
    }

    #[test]
    fn mixed_text_vector_and_uses_robust_pooling() {
        let tree =
            compile_query_string("body:search AND embedding:[0.1, 0.9]", Some("_all")).unwrap();
        assert!(matches!(
            tree,
            OperatorTree::RobustPositiveEvidencePool {
                alpha: 0.5,
                gating: GatingSpec::Softplus,
                ..
            }
        ));
    }

    #[test]
    fn all_field_is_resolved_at_the_engine_boundary() {
        let tree = compile_query_string("database", Some("_all")).unwrap();
        assert!(matches!(tree, OperatorTree::Term { field: None, .. }));
    }
}
