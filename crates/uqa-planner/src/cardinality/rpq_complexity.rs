//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RPQ expression complexity estimation.

/// Count label-bearing tokens in an RPQ expression source. Mirrors
/// Count label nodes in an RPQ expression. Mirrors the canonical UQA implementation's
/// `uqa.planner.cost_model._expr_label_count`: parses the source,
/// then walks the AST. Label = 1, Concat/Alt = sum, KleeneStar =
/// inner * 2, Bounded = inner * max_hops. Falls back to 1 when the
/// source can't be parsed.
pub(super) fn rpq_label_count(source: &str) -> usize {
    match uqa_graph::parse_rpq(source) {
        Ok(expr) => count_rpq_labels(&expr).max(1),
        Err(_) => 1,
    }
}

fn count_rpq_labels(expr: &uqa_graph::RegularPathExpr) -> usize {
    use uqa_graph::RegularPathExpr;
    match expr {
        RegularPathExpr::Label(_) => 1,
        RegularPathExpr::Concat(l, r) | RegularPathExpr::Alternation(l, r) => {
            count_rpq_labels(l) + count_rpq_labels(r)
        }
        RegularPathExpr::KleeneStar(inner) => count_rpq_labels(inner).saturating_mul(2),
        RegularPathExpr::Bounded { inner, max, .. } => {
            count_rpq_labels(inner).saturating_mul(usize::try_from(*max).unwrap_or(usize::MAX))
        }
    }
}
