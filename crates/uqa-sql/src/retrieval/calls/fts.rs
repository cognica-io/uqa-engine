//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FTS syntax lowering to logical retrieval expressions.

use super::super::RetrievalExpr;
use crate::{FTSNode, SQLError};

const VECTOR_K: usize = 10_000;

pub(super) fn compile_query_string(
    query: &str,
    default_field: Option<&str>,
) -> Result<RetrievalExpr, SQLError> {
    let ast = crate::parse_fts_query_string(query)?;
    Ok(compile(&ast, default_field))
}

fn compile(node: &FTSNode, default_field: Option<&str>) -> RetrievalExpr {
    match node {
        FTSNode::Term { field, term } => {
            term_operator(term.clone(), resolve_field(field.as_deref(), default_field))
        }
        FTSNode::Phrase { field, phrase } => RetrievalExpr::Phrase {
            query: phrase.clone(),
            field: resolve_field(field.as_deref(), default_field),
            scoring: None,
        },
        FTSNode::Vector { field, values } => RetrievalExpr::KNN {
            query_vector: values.clone(),
            k: VECTOR_K,
            field: resolve_field(field.as_deref(), default_field)
                .unwrap_or_else(|| "embedding".into()),
        },
        FTSNode::And(left, right) => compile_and(left, right, default_field),
        FTSNode::Or(left, right) => RetrievalExpr::Union(vec![
            compile(left, default_field),
            compile(right, default_field),
        ]),
        FTSNode::Not(operand) => {
            RetrievalExpr::Complement(Box::new(compile(operand, default_field)))
        }
    }
}

fn compile_and(left: &FTSNode, right: &FTSNode, default_field: Option<&str>) -> RetrievalExpr {
    let mut conjuncts = Vec::new();
    collect_conjuncts(left, &mut conjuncts);
    collect_conjuncts(right, &mut conjuncts);

    let can_fuse = conjuncts
        .iter()
        .all(|conjunct| is_text_query_node(conjunct) || matches!(conjunct, FTSNode::Vector { .. }));
    let has_text = conjuncts
        .iter()
        .any(|conjunct| is_text_query_node(conjunct));
    let has_vector = conjuncts
        .iter()
        .any(|conjunct| matches!(conjunct, FTSNode::Vector { .. }));
    if can_fuse && has_text && has_vector {
        let text_trees = conjuncts
            .iter()
            .filter(|conjunct| is_text_query_node(conjunct))
            .map(|conjunct| compile(conjunct, default_field))
            .collect();
        let mut signals = vec![intersect_or_single(text_trees)];
        signals.extend(
            conjuncts
                .iter()
                .filter(|conjunct| matches!(conjunct, FTSNode::Vector { .. }))
                .map(|conjunct| compile(conjunct, default_field)),
        );
        return RetrievalExpr::BayesianEvidenceFusion {
            signals,
            base_rate: None,
        };
    }

    RetrievalExpr::Intersect(vec![
        compile(left, default_field),
        compile(right, default_field),
    ])
}

fn collect_conjuncts<'a>(node: &'a FTSNode, output: &mut Vec<&'a FTSNode>) {
    if let FTSNode::And(left, right) = node {
        collect_conjuncts(left, output);
        collect_conjuncts(right, output);
    } else {
        output.push(node);
    }
}

fn intersect_or_single(mut trees: Vec<RetrievalExpr>) -> RetrievalExpr {
    if trees.len() == 1 {
        trees.pop().expect("one text tree exists")
    } else {
        RetrievalExpr::Intersect(trees)
    }
}

fn term_operator(query: String, field: Option<String>) -> RetrievalExpr {
    RetrievalExpr::Term {
        query,
        field,
        scoring: None,
    }
}

fn resolve_field(node_field: Option<&str>, default_field: Option<&str>) -> Option<String> {
    match node_field.or(default_field) {
        Some("_all") | None => None,
        Some(field) => Some(field.to_string()),
    }
}

fn is_text_query_node(node: &FTSNode) -> bool {
    match node {
        FTSNode::Vector { .. } => false,
        FTSNode::Term { .. } | FTSNode::Phrase { .. } => true,
        FTSNode::And(left, right) | FTSNode::Or(left, right) => {
            is_text_query_node(left) && is_text_query_node(right)
        }
        FTSNode::Not(inner) => is_text_query_node(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_lowering_preserves_complete_input() {
        let tree = compile_query_string("body:\"Rust Ferris Crab\"", None).unwrap();
        assert!(matches!(tree, RetrievalExpr::Phrase {
            query, field: Some(field), scoring: None,
        } if query == "Rust Ferris Crab" && field == "body"));
    }

    #[test]
    fn mixed_text_vector_and_uses_exact_single_prior_fusion() {
        let tree =
            compile_query_string("body:search AND embedding:[0.1, 0.9]", Some("_all")).unwrap();
        assert!(matches!(
            tree,
            RetrievalExpr::BayesianEvidenceFusion {
                base_rate: None,
                ..
            }
        ));
    }

    #[test]
    fn mixed_conjunction_calibrates_the_complete_text_query_once() {
        let tree = compile_query_string(
            "body:search AND body:database AND embedding:[0.1, 0.9]",
            Some("_all"),
        )
        .unwrap();
        let RetrievalExpr::BayesianEvidenceFusion { signals, .. } = tree else {
            panic!("exact hybrid fusion expected");
        };
        assert_eq!(signals.len(), 2);
        assert!(matches!(&signals[0], RetrievalExpr::Intersect(parts) if parts.len() == 2));
        assert!(matches!(&signals[1], RetrievalExpr::KNN { .. }));
    }

    #[test]
    fn all_field_is_resolved_during_logical_lowering() {
        let tree = compile_query_string("database", Some("_all")).unwrap();
        assert!(matches!(tree, RetrievalExpr::Term { field: None, .. }));
    }
}
