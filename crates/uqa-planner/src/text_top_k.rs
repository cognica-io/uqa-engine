//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical planning for score-ordered text limits.

use uqa_operators::{OperatorTree, TextTopKPlan, TextTopKStrategy};

/// Storage and query facts needed to choose an exact text top-k algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextTopKCapabilities {
    /// Number of analyzed query-term occurrences. Occurrences, rather than
    /// unique terms, matter because duplicate query terms contribute twice.
    pub analyzed_term_count: usize,
    pub indexed_document_count: u64,
    /// Every non-empty query posting has persisted bounds matching the active
    /// BM25 parameters and field statistics.
    pub valid_block_max: bool,
}

/// Push a score limit into a simple, field-bound text leaf.
///
/// Boolean/fusion trees deliberately remain exhaustive: cutting a child before
/// its parent changes the carrier. Single-term and effectively unbounded
/// searches also remain exhaustive because WAND cannot prune them profitably.
#[must_use]
pub fn plan_text_top_k(
    tree: OperatorTree,
    k: usize,
    capabilities: TextTopKCapabilities,
) -> OperatorTree {
    let (query, field, scoring, top_k) = match tree {
        OperatorTree::Term {
            query,
            field,
            scoring,
            top_k,
        } => (query, field, scoring, top_k),
        other => return other,
    };

    let eligible = field.is_some()
        && scoring.is_some()
        && top_k.is_none()
        && capabilities.analyzed_term_count >= 2
        && (k == 0 || (k as u128) < u128::from(capabilities.indexed_document_count));
    if !eligible {
        return OperatorTree::Term {
            query,
            field,
            scoring,
            top_k,
        };
    }

    let strategy = if capabilities.valid_block_max {
        TextTopKStrategy::BlockMaxWand
    } else {
        TextTopKStrategy::Wand
    };
    OperatorTree::Term {
        query,
        field,
        scoring,
        top_k: Some(TextTopKPlan { k, strategy }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_operators::TextScoringMode;

    fn term() -> OperatorTree {
        OperatorTree::Term {
            query: "rust search".into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BM25),
            top_k: None,
        }
    }

    #[test]
    fn chooses_bmw_only_for_version_matched_blocks() {
        let planned = plan_text_top_k(
            term(),
            10,
            TextTopKCapabilities {
                analyzed_term_count: 2,
                indexed_document_count: 100,
                valid_block_max: true,
            },
        );
        assert!(matches!(
            planned,
            OperatorTree::Term {
                top_k: Some(TextTopKPlan {
                    strategy: TextTopKStrategy::BlockMaxWand,
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn single_term_and_unbounded_inputs_stay_exhaustive() {
        for (term_count, k, documents) in [(1, 10, 100), (2, 100, 100)] {
            let planned = plan_text_top_k(
                term(),
                k,
                TextTopKCapabilities {
                    analyzed_term_count: term_count,
                    indexed_document_count: documents,
                    valid_block_max: true,
                },
            );
            assert!(matches!(planned, OperatorTree::Term { top_k: None, .. }));
        }
    }
}
